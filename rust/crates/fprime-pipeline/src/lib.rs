//! Decoder/encoder pipeline keyed off the F´ JSON dictionary.
//!
//! The pipeline operates on the inner payload of a deframed F´ frame — the
//! bytes that the FSW's framer/deframer hands to (or receives from) the
//! `FprimeRouter` / CCSDS `SpacePacketDeframer`.  Each packet starts with a
//! 2-byte big-endian `FwPacketDescriptorType` identifying the packet kind
//! (the same value the FSW carries in `ComCfg::FrameContext::apid`):
//!
//! | Descriptor | Meaning             |
//! |------------|---------------------|
//! | `0x0000`   | Command (uplink)    |
//! | `0x0001`   | Telemetry (channel) |
//! | `0x0002`   | Log (event)         |
//! | `0x0004`   | Packetized telemetry|
//! | `0x00FE`   | Handshake           |
//!
//! Note: the Python GDS' `cmd_encoder` prepends a `0x5A5A5A5A | length` pair
//! to outgoing commands.  That pair is *internal middleware framing* used
//! between the GDS comm process and the GDS Tcp server — the FSW never sees
//! it, so we don't emit it here.

#![deny(rust_2018_idioms)]

use fprime_dict::{Channel, Command, Dictionary, Event, FormalParam, TlmPacket};
use fprime_types::{Serde, TimeType, TypeError, Value};
use thiserror::Error;

mod typed;
pub use typed::deserialize_typed;

pub const DESC_COMMAND: u16 = 0x0000;
pub const DESC_TELEM: u16 = 0x0001;
pub const DESC_LOG: u16 = 0x0002;
pub const DESC_PACKETIZED_TLM: u16 = 0x0004;
pub const DESC_HANDSHAKE: u16 = 0x00FE;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("packet too short for descriptor")]
    NoDescriptor,
    #[error("unknown event id {0}")]
    UnknownEvent(u32),
    #[error("unknown channel id {0}")]
    UnknownChannel(u32),
    #[error("unknown packetized telemetry packet id {0}")]
    UnknownTlmPacket(u16),
    #[error("unsupported argument type kind={kind} name={name}")]
    UnsupportedType { kind: String, name: String },
    #[error("argument count mismatch for command {name}: expected {expected}, got {got}")]
    ArgCount {
        name: String,
        expected: usize,
        got: usize,
    },
    #[error("failed to parse command argument {arg} ({ty}): {msg}")]
    ArgParse {
        arg: String,
        ty: String,
        msg: String,
    },
    #[error(transparent)]
    Type(#[from] TypeError),
}

/// A decoded downlink event.
#[derive(Debug, Clone)]
pub struct DecodedEvent {
    pub time: TimeType,
    pub event: Event,
    pub args: Vec<Value>,
}

/// A decoded downlink channel update.
#[derive(Debug, Clone)]
pub struct DecodedChannel {
    pub time: TimeType,
    pub channel: Channel,
    pub value: Value,
}

/// One channel value extracted from a packetized-telemetry packet.  All
/// channels in the same packet share the same `time` (the packet timestamp).
#[derive(Debug, Clone)]
pub struct PacketizedChannel {
    pub channel: Channel,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct DecodedTlmPacket {
    pub packet: TlmPacket,
    pub time: TimeType,
    pub channels: Vec<PacketizedChannel>,
}

/// Anything the downlink pipeline can produce.
#[derive(Debug, Clone)]
pub enum Decoded {
    Event(DecodedEvent),
    Channel(DecodedChannel),
    Handshake(Vec<u8>),
    /// Packetized telemetry: one or more channel values bundled with a single
    /// packet id and timestamp.
    PacketizedTelem(DecodedTlmPacket),
    Unknown {
        descriptor: u16,
        body: Vec<u8>,
    },
}

/// Decode a single F´ deframed packet against the dictionary.
///
/// Returns the first record in the packet \u2014 callers that need to handle the
/// full Space Packet body (which can contain multiple concatenated records
/// after the leading descriptor) should use [`decode_packets`] instead.
pub fn decode_packet(packet: &[u8], dict: &Dictionary) -> Result<Decoded, PipelineError> {
    let mut iter = decode_packets(packet, dict);
    iter.next().ok_or(PipelineError::NoDescriptor)?
}

/// Decode every record in a packet body.
///
/// On the wire, an SP body is `descriptor(U16) | record_0 | record_1 | \u2026`
/// where each record (for events/channels) is `id(U32) | time(11) | value`.
/// The FSW concatenates as many records as fit into one Space Packet, all
/// sharing one descriptor (and APID).  This iterator walks them and yields a
/// [`Decoded`] per record.
///
/// Handshake and packetized-telemetry packets contain a single record each.
pub fn decode_packets<'a>(
    packet: &'a [u8],
    dict: &'a Dictionary,
) -> impl Iterator<Item = Result<Decoded, PipelineError>> + 'a {
    PacketIter {
        dict,
        body: packet,
        descriptor: None,
        done: false,
    }
}

struct PacketIter<'a> {
    dict: &'a Dictionary,
    body: &'a [u8],
    descriptor: Option<u16>,
    done: bool,
}

impl Iterator for PacketIter<'_> {
    type Item = Result<Decoded, PipelineError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let descriptor = match self.descriptor {
            Some(d) => d,
            None => {
                if self.body.len() < 2 {
                    self.done = true;
                    return Some(Err(PipelineError::NoDescriptor));
                }
                let (d, n) = match u16::deserialize(self.body, 0) {
                    Ok(v) => v,
                    Err(e) => {
                        self.done = true;
                        return Some(Err(e.into()));
                    }
                };
                self.body = &self.body[n..];
                self.descriptor = Some(d);
                d
            }
        };

        if self.body.is_empty() {
            return None;
        }

        let (record, consumed) = match descriptor {
            DESC_LOG => match decode_event(self.body, self.dict) {
                Ok((e, n)) => (Decoded::Event(e), n),
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            },
            DESC_TELEM => match decode_channel(self.body, self.dict) {
                Ok((c, n)) => (Decoded::Channel(c), n),
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            },
            DESC_HANDSHAKE => {
                let body = self.body.to_vec();
                self.done = true;
                return Some(Ok(Decoded::Handshake(body)));
            }
            DESC_PACKETIZED_TLM => match decode_tlm_packet(self.body, self.dict) {
                Ok(pkt) => {
                    self.done = true;
                    return Some(Ok(Decoded::PacketizedTelem(pkt)));
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            },
            other => {
                let body = self.body.to_vec();
                self.done = true;
                return Some(Ok(Decoded::Unknown {
                    descriptor: other,
                    body,
                }));
            }
        };
        self.body = &self.body[consumed..];
        Some(Ok(record))
    }
}

/// Width of `FwPacketDescriptorType` on the wire (in bytes).  Matches the F´
/// default and the `dictionary type FwPacketDescriptorType = U16` declared in
/// `default/config/ComCfg.fpp`.
pub const DESCRIPTOR_SIZE: usize = std::mem::size_of::<u16>();

fn decode_event(body: &[u8], dict: &Dictionary) -> Result<(DecodedEvent, usize), PipelineError> {
    let (id, n_id) = u32::deserialize(body, 0)?;
    let (time, n_time) = TimeType::deserialize(body, n_id)?;
    let event = dict
        .events_by_id
        .get(&id)
        .ok_or(PipelineError::UnknownEvent(id))?
        .clone();
    let (args, consumed_args) = decode_args(&event.params, dict, body, n_id + n_time)?;
    Ok((
        DecodedEvent { time, event, args },
        n_id + n_time + consumed_args,
    ))
}

fn decode_channel(
    body: &[u8],
    dict: &Dictionary,
) -> Result<(DecodedChannel, usize), PipelineError> {
    let (id, n_id) = u32::deserialize(body, 0)?;
    let (time, n_time) = TimeType::deserialize(body, n_id)?;
    let channel = dict
        .channels_by_id
        .get(&id)
        .ok_or(PipelineError::UnknownChannel(id))?
        .clone();
    let (value, n_val) = deserialize_typed(&channel.ty, dict, body, n_id + n_time)?;
    Ok((
        DecodedChannel {
            time,
            channel,
            value,
        },
        n_id + n_time + n_val,
    ))
}

/// Decode a packetized-telemetry packet body.
///
/// Wire layout (matches `fprime_gds.common.decoders.pkt_decoder`):
///
/// ```text
/// | FwTlmPacketizeIdType (U16) | TimeType (11 bytes) | val_1 | val_2 | ... |
/// ```
///
/// Each `val_N` is encoded with no per-channel id/time \u2014 the packet template
/// in `dict.tlm_packets_by_id` lists the channels in declaration order.
fn decode_tlm_packet(body: &[u8], dict: &Dictionary) -> Result<DecodedTlmPacket, PipelineError> {
    let (pkt_id, n_id) = u16::deserialize(body, 0)?;
    let (time, n_time) = TimeType::deserialize(body, n_id)?;
    let packet = dict
        .tlm_packets_by_id
        .get(&pkt_id)
        .ok_or(PipelineError::UnknownTlmPacket(pkt_id))?
        .clone();
    let mut cursor = n_id + n_time;
    let mut channels = Vec::with_capacity(packet.channel_ids.len());
    for ch_id in &packet.channel_ids {
        let channel = dict
            .channels_by_id
            .get(ch_id)
            .ok_or(PipelineError::UnknownChannel(*ch_id))?
            .clone();
        let (value, n) = deserialize_typed(&channel.ty, dict, body, cursor)?;
        cursor += n;
        channels.push(PacketizedChannel { channel, value });
    }
    Ok(DecodedTlmPacket {
        packet,
        time,
        channels,
    })
}

fn decode_args(
    params: &[FormalParam],
    dict: &Dictionary,
    body: &[u8],
    start: usize,
) -> Result<(Vec<Value>, usize), PipelineError> {
    let mut offset = start;
    let mut out = Vec::with_capacity(params.len());
    for p in params {
        let (value, n) = deserialize_typed(&p.ty, dict, body, offset)?;
        offset += n;
        out.push(value);
    }
    Ok((out, offset - start))
}

/// Encode a command (the F´ packet body to be handed to the framer).
///
/// Wire format (the bytes the FSW deframer / `FprimeRouter` see, *before* the
/// outer transport framer adds its own header/trailer):
///
/// `U16 desc(=FW_PACKET_COMMAND=0) | U32 opcode | args...`
///
/// We do **not** prepend the `0x5A5A5A5A` magic + U32 length pair that the
/// Python GDS `cmd_encoder` produces.  That pair is internal GDS middleware
/// framing between the comm process and the local Tcp server; it gets stripped
/// before reaching the FSW and must not appear on the FSW-bound wire.
pub fn encode_command(command: &Command, args: &[Value]) -> Result<Vec<u8>, PipelineError> {
    if args.len() != command.params.len() {
        return Err(PipelineError::ArgCount {
            name: command.name.clone(),
            expected: command.params.len(),
            got: args.len(),
        });
    }

    let mut arg_data = Vec::new();
    for (param, value) in command.params.iter().zip(args.iter()) {
        let prim = param
            .ty
            .primitive_name()
            .ok_or_else(|| PipelineError::UnsupportedType {
                kind: param.ty.kind.clone(),
                name: param.ty.name.clone(),
            })?;
        if !type_matches(prim, value) {
            return Err(PipelineError::ArgParse {
                arg: param.name.clone(),
                ty: prim.to_owned(),
                msg: format!("got {}", value.type_name()),
            });
        }
        value.serialize(&mut arg_data);
    }

    let mut out = Vec::with_capacity(DESCRIPTOR_SIZE + 4 + arg_data.len());
    out.extend_from_slice(&DESC_COMMAND.to_be_bytes());
    out.extend_from_slice(&command.opcode.to_be_bytes());
    out.extend_from_slice(&arg_data);
    Ok(out)
}

fn type_matches(prim: &str, v: &Value) -> bool {
    matches!(
        (prim, v),
        ("bool", Value::Bool(_))
            | ("U8", Value::U8(_))
            | ("U16", Value::U16(_))
            | ("U32", Value::U32(_))
            | ("U64", Value::U64(_))
            | ("I8", Value::I8(_))
            | ("I16", Value::I16(_))
            | ("I32", Value::I32(_))
            | ("I64", Value::I64(_))
            | ("F32", Value::F32(_))
            | ("F64", Value::F64(_))
            | ("string", Value::String(_))
    )
}

/// Parse a single string argument as the requested primitive type.  Used by
/// the REPL so that users can type `cmd Foo 42 "hello" 3.14`.
pub fn parse_arg(prim: &str, raw: &str) -> Result<Value, PipelineError> {
    let r = match prim {
        "bool" => Value::Bool(parse_bool(raw)?),
        "U8" => Value::U8(parse_int(raw)?),
        "U16" => Value::U16(parse_int(raw)?),
        "U32" => Value::U32(parse_int(raw)?),
        "U64" => Value::U64(parse_int(raw)?),
        "I8" => Value::I8(parse_int(raw)?),
        "I16" => Value::I16(parse_int(raw)?),
        "I32" => Value::I32(parse_int(raw)?),
        "I64" => Value::I64(parse_int(raw)?),
        "F32" => Value::F32(
            raw.parse()
                .map_err(|e: std::num::ParseFloatError| arg_err(prim, raw, &e.to_string()))?,
        ),
        "F64" => Value::F64(
            raw.parse()
                .map_err(|e: std::num::ParseFloatError| arg_err(prim, raw, &e.to_string()))?,
        ),
        "string" => {
            if raw.len() > u16::MAX as usize {
                return Err(arg_err(
                    prim,
                    &format!("<{} bytes>", raw.len()),
                    &format!("string exceeds {} bytes", u16::MAX),
                ));
            }
            Value::String(fprime_types::FpString(raw.to_owned()))
        }
        other => {
            return Err(PipelineError::UnsupportedType {
                kind: other.to_owned(),
                name: other.to_owned(),
            })
        }
    };
    Ok(r)
}

fn parse_int<I>(raw: &str) -> Result<I, PipelineError>
where
    I: std::str::FromStr<Err = std::num::ParseIntError>,
    I: num_from_radix::FromRadix,
{
    if let Some(stripped) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        I::from_radix(stripped, 16).map_err(|e| arg_err("int", raw, &e.to_string()))
    } else {
        raw.parse()
            .map_err(|e: std::num::ParseIntError| arg_err("int", raw, &e.to_string()))
    }
}

fn parse_bool(raw: &str) -> Result<bool, PipelineError> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(arg_err("bool", other, "expected true/false")),
    }
}

fn arg_err(ty: &str, raw: &str, msg: &str) -> PipelineError {
    PipelineError::ArgParse {
        arg: raw.to_owned(),
        ty: ty.to_owned(),
        msg: msg.to_owned(),
    }
}

mod num_from_radix {
    pub trait FromRadix: Sized {
        fn from_radix(src: &str, radix: u32) -> Result<Self, std::num::ParseIntError>;
    }
    macro_rules! impl_from_radix {
        ($($t:ty),*) => {
            $(impl FromRadix for $t {
                fn from_radix(src: &str, radix: u32) -> Result<Self, std::num::ParseIntError> {
                    <$t>::from_str_radix(src, radix)
                }
            })*
        };
    }
    impl_from_radix!(u8, u16, u32, u64, i8, i16, i32, i64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fprime_dict::TypeRef;

    fn u32_type() -> TypeRef {
        TypeRef {
            name: "U32".into(),
            kind: "integer".into(),
            size: Some(32),
            signed: Some(false),
        }
    }

    fn make_dict_with_event() -> Dictionary {
        let mut dict = Dictionary::default();
        dict.events_by_id.insert(
            42,
            Event {
                id: 42,
                name: "Test.E".into(),
                severity: "INFO".into(),
                format: "x = {}".into(),
                params: vec![FormalParam {
                    name: "x".into(),
                    ty: u32_type(),
                    annotation: None,
                }],
                annotation: None,
            },
        );
        dict
    }

    #[test]
    fn decode_event_packet() {
        let dict = make_dict_with_event();
        // descriptor (LOG, U16) | id (U32) | time | x
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&DESC_LOG.to_be_bytes());
        bytes.extend_from_slice(&42u32.to_be_bytes());
        TimeType::default().serialize(&mut bytes);
        bytes.extend_from_slice(&123u32.to_be_bytes());
        let decoded = decode_packet(&bytes, &dict).unwrap();
        match decoded {
            Decoded::Event(e) => {
                assert_eq!(e.event.name, "Test.E");
                assert_eq!(e.args, vec![Value::U32(123)]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_no_arg_command() {
        let cmd = Command {
            opcode: 1280,
            name: "Ref.cmdDisp.CMD_NO_OP".into(),
            params: vec![],
            annotation: None,
        };
        let body = encode_command(&cmd, &[]).unwrap();
        // U16 desc(=0) | U32 opcode
        assert_eq!(body.len(), DESCRIPTOR_SIZE + 4);
        assert_eq!(&body[0..2], &DESC_COMMAND.to_be_bytes());
        assert_eq!(&body[2..6], &1280u32.to_be_bytes());
    }

    #[test]
    fn decode_packets_yields_multiple_records_per_body() {
        // The FSW concatenates several `id|time|value` records under a single
        // descriptor inside one Space Packet.  Verify we yield each one.
        let mut dict = Dictionary::default();
        for id in [10u32, 11, 12] {
            dict.channels_by_id.insert(
                id,
                Channel {
                    id,
                    name: format!("ch{id}"),
                    ty: u32_type(),
                    annotation: None,
                },
            );
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&DESC_TELEM.to_be_bytes());
        for (id, val) in [(10u32, 100u32), (11, 200), (12, 300)] {
            bytes.extend_from_slice(&id.to_be_bytes());
            TimeType::default().serialize(&mut bytes);
            bytes.extend_from_slice(&val.to_be_bytes());
        }
        let decoded: Vec<_> = decode_packets(&bytes, &dict).map(|r| r.unwrap()).collect();
        assert_eq!(decoded.len(), 3);
        for (i, d) in decoded.iter().enumerate() {
            match d {
                Decoded::Channel(c) => {
                    assert_eq!(c.channel.name, format!("ch{}", 10 + i));
                    assert_eq!(c.value, Value::U32(((i + 1) * 100) as u32));
                }
                _ => panic!("expected channel"),
            }
        }
    }

    #[test]
    fn decode_unknown_descriptor_is_u16() {
        let dict = Dictionary::default();
        // bytes 0x00 0x10 | rest
        let bytes = [0x00, 0x10, 0xAA, 0xBB, 0xCC];
        match decode_packet(&bytes, &dict).unwrap() {
            Decoded::Unknown { descriptor, body } => {
                assert_eq!(descriptor, 0x0010);
                assert_eq!(body, vec![0xAA, 0xBB, 0xCC]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_int_hex_and_decimal() {
        assert_eq!(parse_arg("U32", "100").unwrap(), Value::U32(100));
        assert_eq!(parse_arg("U32", "0xFF").unwrap(), Value::U32(255));
    }
}
