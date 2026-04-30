//! Decoder/encoder pipeline keyed off the F´ JSON dictionary.
//!
//! The pipeline operates on [`Packet`]s — the inner payload of a deframed F´
//! frame.  Each packet starts with a 4-byte big-endian descriptor identifying
//! the packet kind:
//!
//! | Descriptor | Meaning             |
//! |------------|---------------------|
//! | `0x00`     | Command (uplink)    |
//! | `0x01`     | Telemetry (channel) |
//! | `0x02`     | Log (event)         |
//! | `0x04`     | Packetized telemetry|
//! | `0xFE`     | Handshake           |

#![deny(rust_2018_idioms)]

use fprime_dict::{Channel, Command, Dictionary, Event, FormalParam};
use fprime_types::{Serde, TimeType, TypeError, Value};
use thiserror::Error;

pub const DESC_COMMAND: u32 = 0x0000_0000;
pub const DESC_TELEM: u32 = 0x0000_0001;
pub const DESC_LOG: u32 = 0x0000_0002;
pub const DESC_PACKETIZED_TLM: u32 = 0x0000_0004;
pub const DESC_HANDSHAKE: u32 = 0x0000_00FE;

/// Magic value the FSW expects in front of an uplinked command.  Matches
/// `fprime_gds.common.encoders.cmd_encoder`.
pub const COMMAND_MAGIC: u32 = 0x5A5A_5A5A;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("packet too short for descriptor")]
    NoDescriptor,
    #[error("unknown event id {0}")]
    UnknownEvent(u32),
    #[error("unknown channel id {0}")]
    UnknownChannel(u32),
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

/// Anything the downlink pipeline can produce.
#[derive(Debug, Clone)]
pub enum Decoded {
    Event(DecodedEvent),
    Channel(DecodedChannel),
    Handshake(Vec<u8>),
    PacketizedTelem(Vec<u8>),
    Unknown { descriptor: u32, body: Vec<u8> },
}

/// Decode a single F´ deframed packet against the dictionary.
pub fn decode_packet(packet: &[u8], dict: &Dictionary) -> Result<Decoded, PipelineError> {
    if packet.len() < 4 {
        return Err(PipelineError::NoDescriptor);
    }
    let (descriptor, _) = u32::deserialize(packet, 0)?;
    let body = &packet[4..];
    match descriptor {
        DESC_LOG => decode_event(body, dict).map(Decoded::Event),
        DESC_TELEM => decode_channel(body, dict).map(Decoded::Channel),
        DESC_HANDSHAKE => Ok(Decoded::Handshake(body.to_vec())),
        DESC_PACKETIZED_TLM => Ok(Decoded::PacketizedTelem(body.to_vec())),
        other => Ok(Decoded::Unknown {
            descriptor: other,
            body: body.to_vec(),
        }),
    }
}

fn decode_event(body: &[u8], dict: &Dictionary) -> Result<DecodedEvent, PipelineError> {
    let (id, n_id) = u32::deserialize(body, 0)?;
    let (time, n_time) = TimeType::deserialize(body, n_id)?;
    let event = dict
        .events_by_id
        .get(&id)
        .ok_or(PipelineError::UnknownEvent(id))?
        .clone();
    let args = decode_args(&event.params, body, n_id + n_time)?;
    Ok(DecodedEvent { time, event, args })
}

fn decode_channel(body: &[u8], dict: &Dictionary) -> Result<DecodedChannel, PipelineError> {
    let (id, n_id) = u32::deserialize(body, 0)?;
    let (time, n_time) = TimeType::deserialize(body, n_id)?;
    let channel = dict
        .channels_by_id
        .get(&id)
        .ok_or(PipelineError::UnknownChannel(id))?
        .clone();
    let prim = channel
        .ty
        .primitive_name()
        .ok_or_else(|| PipelineError::UnsupportedType {
            kind: channel.ty.kind.clone(),
            name: channel.ty.name.clone(),
        })?;
    let (value, _n) = Value::deserialize_named(prim, body, n_id + n_time)?;
    Ok(DecodedChannel {
        time,
        channel,
        value,
    })
}

fn decode_args(
    params: &[FormalParam],
    body: &[u8],
    mut offset: usize,
) -> Result<Vec<Value>, PipelineError> {
    let mut out = Vec::with_capacity(params.len());
    for p in params {
        let prim =
            p.ty.primitive_name()
                .ok_or_else(|| PipelineError::UnsupportedType {
                    kind: p.ty.kind.clone(),
                    name: p.ty.name.clone(),
                })?;
        let (value, n) = Value::deserialize_named(prim, body, offset)?;
        offset += n;
        out.push(value);
    }
    Ok(out)
}

/// Encode a command (the F´ packet body to be framed and sent over uplink).
///
/// On the wire:
/// `U32 0x5A5A5A5A | U32 length | U32 desc(=0) | U32 opcode | args...`
///
/// `length` covers the descriptor + opcode + args, matching `cmd_encoder.py`.
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

    let descriptor: u32 = DESC_COMMAND;
    let length = (4 /* desc */ + 4 /* opcode */ + arg_data.len()) as u32;

    let mut out = Vec::with_capacity(16 + arg_data.len());
    out.extend_from_slice(&COMMAND_MAGIC.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&descriptor.to_be_bytes());
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
        "string" => Value::String(fprime_types::FpString(raw.to_owned())),
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
        // descriptor (LOG) | id | time | x
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
        // magic | length(=8) | desc | opcode
        assert_eq!(&body[0..4], &COMMAND_MAGIC.to_be_bytes());
        let length = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        assert_eq!(length, 8);
        assert_eq!(&body[8..12], &DESC_COMMAND.to_be_bytes());
        assert_eq!(&body[12..16], &1280u32.to_be_bytes());
    }

    #[test]
    fn parse_int_hex_and_decimal() {
        assert_eq!(parse_arg("U32", "100").unwrap(), Value::U32(100));
        assert_eq!(parse_arg("U32", "0xFF").unwrap(), Value::U32(255));
    }
}
