//! F Prime telemetry: the decoders of channel packets and of telemetry packets (both
//! produce kind `channel`, one record per sample) and the channel definitions route.

use fgds_core::axum::extract::State;
use fgds_core::axum::routing::get;
use fgds_core::axum::{Json, Router};
use fgds_core::dictionary::{Channel, Reader, Type, TypeError};
use fgds_core::serde_json::{json, Value};
use fgds_core::{
    Access, Context, DecodeError, Decoder, Dictionary, FieldSpec, FieldType, FlightTime, KindSpec, Manifest, NewRecord,
    PacketMatch, PacketMeta, PacketType, Plugin, Registry, Result, RouteAccess, Routes,
};
use std::sync::Arc;

pub const KIND: &str = "fprime.channel";

pub struct ChannelsPlugin;

impl Plugin for ChannelsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "channels", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec {
            fields: &[FieldSpec { name: "name", pointer: "/name", ty: FieldType::Text }],
            text_fields: &["/name"],
            ..KindSpec::new(KIND, "A telemetry channel sample; id is the channel identifier; raw is identifier, time tag, value")
        })?;
        reg.add_decoder(Arc::new(|_| Ok(Box::new(ChannelDecoder))))?;
        reg.add_decoder(Arc::new(|_| Ok(Box::new(PacketDecoder))))?;
        reg.add_routes(Routes {
            prefix: "channels",
            router: Router::new().route("/definitions", get(definitions)),
            access: vec![RouteAccess { method: "GET", path: "/definitions", access: Access::Read }],
        })
    }
}

/// The channels the dictionary defines.
async fn definitions(State(ctx): State<Context>) -> Json<Value> {
    let channels = ctx.dictionary.channels.iter();
    Json(channels.map(|c| json!({"id": c.id, "name": c.name, "format": c.format})).collect())
}

fn error(e: TypeError) -> DecodeError {
    DecodeError(e.0)
}

/// Read an unsigned integer of the width the dictionary gives `type_name`.
fn uint(r: &mut Reader, dict: &Dictionary, type_name: &str, default_bits: u8) -> Result<u32, DecodeError> {
    let bits = match dict.types.get(type_name) {
        Some(Type::Int { bits, .. }) => *bits,
        _ => default_bits,
    };
    Ok(r.uint(bits).map_err(error)? as u32)
}

fn time(r: &mut Reader, dict: &Dictionary) -> Result<FlightTime, DecodeError> {
    let t = dict.decode_time(r).map_err(error)?;
    Ok(FlightTime { base: t.base, context: t.context, seconds: t.seconds, microseconds: t.microseconds })
}

/// One sample: the value and its display text, as the Python GDS shows it.
fn sample(channel: &Channel, time: FlightTime, r: &mut Reader, dict: &Dictionary) -> Result<NewRecord, DecodeError> {
    let value = dict.decode(&channel.ty, r).map_err(|e| DecodeError(format!("channel {}: {}", channel.name, e.0)))?;
    let display_text = match &channel.format {
        Some(format) => dict.format(format, &[(&channel.ty, &value)]),
        None => dict.display_channel(&channel.ty, &value),
    };
    Ok(NewRecord {
        id: Some(channel.id),
        flight_time: Some(time),
        ..NewRecord::new(KIND, json!({"name": channel.name, "value": value, "display_text": display_text}))
    })
}

pub struct ChannelDecoder;

impl Decoder for ChannelDecoder {
    fn matches(&self) -> PacketMatch {
        PacketMatch { packet_type: Some(PacketType::TELEMETRY), meta: vec![] }
    }

    /// Packet: packet type, then one or more of: channel identifier, time tag, value.
    /// `raw` of each record is that sample: channel identifier, time tag, value.
    /// The flight software packs several samples into one packet when they fit.
    fn decode(&self, packet: &[u8], _: &PacketMeta, dict: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        let mut r = Reader::new(packet);
        uint(&mut r, dict, "FwPacketDescriptorType", 16)?;
        let mut records = Vec::new();
        loop {
            let start = packet.len() - r.remaining();
            let id = uint(&mut r, dict, "FwChanIdType", 32)?;
            let time = time(&mut r, dict)?;
            let channel =
                dict.channel(id).ok_or_else(|| DecodeError(format!("channel {id} is not in the dictionary")))?;
            let mut record = sample(channel, time, &mut r, dict)?;
            let end = packet.len() - r.remaining();
            // The sample's own bytes: channel identifier, time tag, value.
            record.raw = fgds_core::bytes::Bytes::copy_from_slice(&packet[start..end]);
            records.push(record);
            if r.remaining() == 0 {
                return Ok(records);
            }
        }
    }
}

pub struct PacketDecoder;

impl Decoder for PacketDecoder {
    fn matches(&self) -> PacketMatch {
        PacketMatch { packet_type: Some(PacketType::PACKETIZED_TELEMETRY), meta: vec![] }
    }

    /// Packet: packet type, telemetry packet identifier, time tag, then the value of
    /// each channel of the packet in a slot of the channel type's largest size.
    fn decode(&self, packet: &[u8], _: &PacketMeta, dict: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        let mut r = Reader::new(packet);
        uint(&mut r, dict, "FwPacketDescriptorType", 16)?;
        let id = uint(&mut r, dict, "FwTlmPacketizeIdType", 16)?;
        let before = r.remaining();
        let time = time(&mut r, dict)?;
        let time_bytes = &packet[packet.len() - before..packet.len() - r.remaining()];
        let channels =
            dict.packet(id).ok_or_else(|| DecodeError(format!("telemetry packet {id} is not in the dictionary")))?;
        let mut records = Vec::new();
        for channel in channels {
            let slot = r.take(dict.max_size(&channel.ty)).map_err(error)?;
            let mut value = Reader::new(slot);
            let mut record = sample(channel, time, &mut value, dict)?;
            // The same layout as a sample of a channel packet, built from this packet.
            let mut raw = Vec::new();
            let id_type = dict.config_type("FwChanIdType", Type::U32);
            dict.encode(&id_type, &json!(channel.id), &mut raw).map_err(error)?;
            raw.extend_from_slice(time_bytes);
            raw.extend_from_slice(&slot[..slot.len() - value.remaining()]);
            record.raw = raw.into();
            records.push(record);
        }
        Ok(records)
    }
}
