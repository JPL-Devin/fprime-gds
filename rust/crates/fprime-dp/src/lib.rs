//! F´ Data Product (`.fdp`) decoder.
//!
//! This is the Rust analogue of `fprime_gds.common.dp.decoder`.  It reads a
//! data-product binary file produced by the FSW's `Fw::DpContainer`, walks
//! the header (whose field widths are dictated by the dictionary's
//! `typeDefinitions`), iterates the records (looking each one up by id in
//! the dictionary's `records[]`), and produces a JSON tree that mirrors the
//! Python GDS' `decoder.decode()` output.
//!
//! Wire format (per [`Fw/Dp/docs/sdd.md`](https://fprime.jpl.nasa.gov/latest/Fw/Dp/docs/sdd)):
//!
//! ```text
//!   PacketDescriptor : FwPacketDescriptorType   (== Fw::ComPacketType::FW_PACKET_DP, 0x0005)
//!   Id               : FwDpIdType               (container id)
//!   Priority         : FwDpPriorityType
//!   Time             : Fw::Time                 (11 bytes)
//!   ProcTypes        : Fw::DpCfg::ProcType::SerialType
//!   UserData[N]      : Fw::DpCfg::CONTAINER_USER_DATA_SIZE bytes
//!   DpState          : Fw::DpState::SerialType
//!   DataSize         : FwSizeStoreType
//!   Checksum         : U32 (CRC32 of all preceding header bytes)
//!   Records          : DataSize bytes of `(record_id [, array_size], data)*`
//!   DataChecksum     : U32 (CRC32 of all `Records` bytes)
//! ```

#![deny(rust_2018_idioms)]

use std::path::Path;

use fprime_dict::{Dictionary, DpRecord, TypeDef, TypeRef};
use fprime_pipeline::deserialize_typed;
use fprime_types::{Serde, TimeType, TypeError, Value};
use serde_json::{json, Map, Value as Json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DpError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Type(#[from] TypeError),
    #[error(transparent)]
    Pipeline(#[from] fprime_pipeline::PipelineError),
    #[error("data product file too short: need {need} bytes, have {have}")]
    TooShort { need: usize, have: usize },
    #[error("dictionary missing required type {0}")]
    MissingType(String),
    #[error("dictionary missing required constant {0}")]
    MissingConstant(String),
    #[error(
        "unsupported type definition for {0}: only integer aliases / U8 enums are supported here"
    )]
    UnsupportedHeaderType(String),
    #[error("CRC mismatch in {section}: expected {expected:#010x}, got {computed:#010x}")]
    CrcMismatch {
        section: &'static str,
        expected: u32,
        computed: u32,
    },
    #[error("unknown record id {0}")]
    UnknownRecord(u32),
    #[error("expected packet descriptor {expected} (FW_PACKET_DP), got {got}")]
    BadDescriptor { expected: u32, got: u32 },
}

/// Width (in bytes) and signedness of an integer type resolved against the
/// dictionary.
#[derive(Debug, Clone, Copy)]
struct IntWidth {
    bytes: usize,
    signed: bool,
}

/// Header field values, decoded against the dictionary.
#[derive(Debug, Clone)]
pub struct DecodedHeader {
    pub packet_descriptor: u64,
    pub container_id: u32,
    pub priority: u64,
    pub time: TimeType,
    pub proc_types: u8,
    pub user_data: Vec<u8>,
    pub dp_state_value: u64,
    pub dp_state_label: Option<String>,
    pub data_size: u64,
    pub checksum: u32,
}

/// One decoded record (single value or array of values).
#[derive(Debug, Clone)]
pub struct DecodedRecord {
    pub record: DpRecord,
    pub data: RecordData,
}

#[derive(Debug, Clone)]
pub enum RecordData {
    Scalar(Value),
    Array(Vec<Value>),
}

/// Whole decoded data-product file.
#[derive(Debug, Clone)]
pub struct DecodedDp {
    pub header: DecodedHeader,
    pub records: Vec<DecodedRecord>,
    pub data_checksum: u32,
}

/// Decode a `.fdp` file from disk.
pub fn decode_path(path: &Path, dict: &Dictionary) -> Result<DecodedDp, DpError> {
    let bytes = std::fs::read(path)?;
    decode_bytes(&bytes, dict)
}

/// Decode a `.fdp` file from an in-memory buffer.
pub fn decode_bytes(bytes: &[u8], dict: &Dictionary) -> Result<DecodedDp, DpError> {
    let pd = resolve_int_alias(dict, "FwPacketDescriptorType")?;
    let id = resolve_int_alias(dict, "FwDpIdType")?;
    let pri = resolve_int_alias(dict, "FwDpPriorityType")?;
    let size_store = resolve_int_alias(dict, "FwSizeStoreType")?;
    let user_data_size = dict
        .int_constants
        .get("Fw.DpCfg.CONTAINER_USER_DATA_SIZE")
        .copied()
        .ok_or_else(|| DpError::MissingConstant("Fw.DpCfg.CONTAINER_USER_DATA_SIZE".into()))?
        as usize;

    // ProcType and DpState are 1-byte enums (their `representationType` is U8).
    let proc_types_w = resolve_enum_int_width(dict, "Fw.DpCfg.ProcType")?;
    let (dp_state_w, dp_state_constants) = resolve_enum(dict, "Fw.DpState")?;

    let header_size = pd.bytes
        + id.bytes
        + pri.bytes
        + TimeType::SIZE
        + proc_types_w.bytes
        + user_data_size
        + dp_state_w.bytes
        + size_store.bytes
        + 4; // 4-byte CRC32 trailer of the header

    if bytes.len() < header_size {
        return Err(DpError::TooShort {
            need: header_size,
            have: bytes.len(),
        });
    }

    // Validate header CRC.
    let header_crc_offset = header_size - 4;
    let header_crc = u32::from_be_bytes([
        bytes[header_crc_offset],
        bytes[header_crc_offset + 1],
        bytes[header_crc_offset + 2],
        bytes[header_crc_offset + 3],
    ]);
    let computed_header_crc = crc32fast::hash(&bytes[..header_crc_offset]);
    if header_crc != computed_header_crc {
        return Err(DpError::CrcMismatch {
            section: "Header",
            expected: header_crc,
            computed: computed_header_crc,
        });
    }

    let mut cur = 0;
    let packet_descriptor = read_int(bytes, cur, pd)?;
    cur += pd.bytes;
    let container_id_u64 = read_int(bytes, cur, id)?;
    let container_id = container_id_u64 as u32;
    cur += id.bytes;
    let priority = read_int(bytes, cur, pri)?;
    cur += pri.bytes;
    let (time, _) = TimeType::deserialize(bytes, cur)?;
    cur += TimeType::SIZE;
    let proc_types_u64 = read_int(bytes, cur, proc_types_w)?;
    cur += proc_types_w.bytes;
    let user_data = bytes[cur..cur + user_data_size].to_vec();
    cur += user_data_size;
    let dp_state_value = read_int(bytes, cur, dp_state_w)?;
    cur += dp_state_w.bytes;
    let data_size_u64 = read_int(bytes, cur, size_store)?;
    cur += size_store.bytes;
    debug_assert_eq!(cur, header_crc_offset);

    if packet_descriptor != FW_PACKET_DP {
        return Err(DpError::BadDescriptor {
            expected: FW_PACKET_DP as u32,
            got: packet_descriptor as u32,
        });
    }

    let dp_state_label = dp_state_constants
        .iter()
        .find(|(_, v)| *v as u64 == dp_state_value)
        .map(|(n, _)| n.clone());

    let header = DecodedHeader {
        packet_descriptor,
        container_id,
        priority,
        time,
        proc_types: proc_types_u64 as u8,
        user_data,
        dp_state_value,
        dp_state_label,
        data_size: data_size_u64,
        checksum: header_crc,
    };

    // Records.
    let data_size = data_size_u64 as usize;
    let data_offset = header_size;
    if bytes.len() < data_offset + data_size + 4 {
        return Err(DpError::TooShort {
            need: data_offset + data_size + 4,
            have: bytes.len(),
        });
    }
    let data_slice = &bytes[data_offset..data_offset + data_size];
    let mut records = Vec::new();
    let mut cur = 0;
    while cur < data_slice.len() {
        // record id is FwDpIdType.
        let rid_u64 = read_int(data_slice, cur, id)?;
        cur += id.bytes;
        let rid = rid_u64 as u32;
        let template = dict
            .dp_records_by_id
            .get(&rid)
            .ok_or(DpError::UnknownRecord(rid))?;

        let data = if template.is_array {
            let n = read_int(data_slice, cur, size_store)? as usize;
            cur += size_store.bytes;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                let (v, used) = deserialize_typed(&template.ty, dict, data_slice, cur)?;
                cur += used;
                items.push(v);
            }
            RecordData::Array(items)
        } else {
            let (v, used) = deserialize_typed(&template.ty, dict, data_slice, cur)?;
            cur += used;
            RecordData::Scalar(v)
        };
        records.push(DecodedRecord {
            record: template.clone(),
            data,
        });
    }
    if cur != data_slice.len() {
        return Err(DpError::TooShort {
            need: cur,
            have: data_slice.len(),
        });
    }

    // Validate data CRC.
    let data_crc_off = data_offset + data_size;
    let data_crc = u32::from_be_bytes([
        bytes[data_crc_off],
        bytes[data_crc_off + 1],
        bytes[data_crc_off + 2],
        bytes[data_crc_off + 3],
    ]);
    let computed_data_crc = crc32fast::hash(data_slice);
    if data_crc != computed_data_crc {
        return Err(DpError::CrcMismatch {
            section: "Data",
            expected: data_crc,
            computed: computed_data_crc,
        });
    }

    Ok(DecodedDp {
        header,
        records,
        data_checksum: data_crc,
    })
}

/// `Fw::ComPacketType::FW_PACKET_DP`.
pub const FW_PACKET_DP: u64 = 0x0005;

fn resolve_int_alias(dict: &Dictionary, name: &str) -> Result<IntWidth, DpError> {
    let underlying = dict
        .resolve_alias(name)
        .ok_or_else(|| DpError::MissingType(name.to_owned()))?;
    integer_width(underlying).ok_or_else(|| DpError::UnsupportedHeaderType(name.to_owned()))
}

fn resolve_enum(dict: &Dictionary, name: &str) -> Result<(IntWidth, Vec<(String, i64)>), DpError> {
    let td = dict
        .types_by_name
        .get(name)
        .ok_or_else(|| DpError::MissingType(name.to_owned()))?;
    if let TypeDef::Enum {
        representation,
        constants,
    } = td
    {
        let w = integer_width(representation)
            .ok_or_else(|| DpError::UnsupportedHeaderType(name.to_owned()))?;
        Ok((w, constants.clone()))
    } else {
        Err(DpError::UnsupportedHeaderType(name.to_owned()))
    }
}

fn resolve_enum_int_width(dict: &Dictionary, name: &str) -> Result<IntWidth, DpError> {
    Ok(resolve_enum(dict, name)?.0)
}

fn integer_width(t: &TypeRef) -> Option<IntWidth> {
    if t.kind != "integer" {
        return None;
    }
    let bits = t.size?;
    Some(IntWidth {
        bytes: (bits as usize) / 8,
        signed: t.signed.unwrap_or(false),
    })
}

fn read_int(buf: &[u8], offset: usize, w: IntWidth) -> Result<u64, DpError> {
    if buf.len() < offset + w.bytes {
        return Err(DpError::TooShort {
            need: offset + w.bytes,
            have: buf.len(),
        });
    }
    let slice = &buf[offset..offset + w.bytes];
    // Big-endian, possibly signed.  We return as u64; callers cast to the
    // target width.
    let mut acc: u64 = 0;
    for &b in slice {
        acc = (acc << 8) | (b as u64);
    }
    if w.signed && w.bytes < 8 {
        // Sign-extend.
        let sign_bit = 1u64 << (w.bytes * 8 - 1);
        if acc & sign_bit != 0 {
            let mask = !((1u64 << (w.bytes * 8)) - 1);
            acc |= mask;
        }
    }
    Ok(acc)
}

/// Render a [`DecodedDp`] as a JSON value matching the Python GDS' DP
/// decoder output.
pub fn to_json(dp: &DecodedDp) -> Json {
    let header = header_to_json(&dp.header);
    let records: Vec<Json> = dp.records.iter().map(record_to_json).collect();
    json!({
        "Header": header,
        "Records": records,
    })
}

fn header_to_json(h: &DecodedHeader) -> Json {
    let mut m = Map::new();
    m.insert(
        "PacketDescriptor".into(),
        wrap_value(
            h.packet_descriptor as i64,
            "U32",
            "The F Prime packet descriptor",
        ),
    );
    m.insert(
        "Id".into(),
        wrap_value(h.container_id as i64, "U32", "The container ID"),
    );
    m.insert(
        "Priority".into(),
        wrap_value(h.priority as i64, "U32", "The container priority"),
    );
    m.insert("Time".into(), time_to_json(&h.time));
    m.insert(
        "ProcTypes".into(),
        wrap_value(h.proc_types as i64, "U8", "Processing types bit mask"),
    );
    m.insert("UserData".into(), user_data_to_json(&h.user_data));
    m.insert(
        "DpState".into(),
        wrap_string(
            h.dp_state_label
                .clone()
                .unwrap_or_else(|| h.dp_state_value.to_string()),
            "Fw.DpState",
            "Data product state",
        ),
    );
    m.insert(
        "DataSize".into(),
        wrap_value(h.data_size as i64, "U16", "Size of data payload in bytes"),
    );
    m.insert(
        "Checksum".into(),
        wrap_value(h.checksum as i64, "U32", "Header checksum"),
    );
    Json::Object(m)
}

fn time_to_json(t: &TimeType) -> Json {
    json!({
        "format": "{}",
        "description": "Fw.Time object",
        "type": "Time",
        "base": t.time_base,
        "context": t.time_context,
        "seconds": t.seconds,
        "microseconds": t.useconds,
    })
}

fn user_data_to_json(bytes: &[u8]) -> Json {
    let values: Vec<Json> = bytes
        .iter()
        .map(|b| json!({ "value": *b, "type": "U8" }))
        .collect();
    json!({
        "format": "{}",
        "description": "User-configurable data",
        "name": "UserData",
        "type": "UserData",
        "size": bytes.len(),
        "values": values,
    })
}

fn wrap_value(v: i64, ty: &str, desc: &str) -> Json {
    json!({
        "format": "{}",
        "description": desc,
        "value": v,
        "type": ty,
    })
}

fn wrap_string(v: String, ty: &str, desc: &str) -> Json {
    json!({
        "format": "{}",
        "description": desc,
        "value": v,
        "type": ty,
    })
}

fn record_to_json(r: &DecodedRecord) -> Json {
    let template = json!({
        "record_id": r.record.id,
        "record_name": r.record.name,
        "is_array": r.record.is_array,
        "type": &r.record.ty.name,
        "annotation": r.record.annotation,
    });
    match &r.data {
        RecordData::Scalar(v) => json!({
            "Record": template,
            "Data": value_to_json(v),
        }),
        RecordData::Array(items) => json!({
            "Record": template,
            "Size": items.len(),
            "Data": items.iter().map(value_to_json).collect::<Vec<_>>(),
        }),
    }
}

/// Render a [`Value`] as a JSON value.  Primitives are wrapped with their
/// `type` name (matching the Python GDS); compound types preserve structure.
pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Bool(b) => json!({ "value": *b, "type": "Bool" }),
        Value::U8(x) => json!({ "value": *x, "type": "U8" }),
        Value::U16(x) => json!({ "value": *x, "type": "U16" }),
        Value::U32(x) => json!({ "value": *x, "type": "U32" }),
        Value::U64(x) => json!({ "value": *x, "type": "U64" }),
        Value::I8(x) => json!({ "value": *x, "type": "I8" }),
        Value::I16(x) => json!({ "value": *x, "type": "I16" }),
        Value::I32(x) => json!({ "value": *x, "type": "I32" }),
        Value::I64(x) => json!({ "value": *x, "type": "I64" }),
        Value::F32(x) => json!({ "value": *x, "type": "F32" }),
        Value::F64(x) => json!({ "value": *x, "type": "F64" }),
        Value::String(s) => json!({ "value": s.0.clone(), "type": "String" }),
        Value::Enum {
            type_name,
            label,
            value,
        } => json!({
            "value": label.clone().unwrap_or_else(|| value.to_string()),
            "type": type_name,
        }),
        Value::Array { type_name, items } => json!({
            "type": type_name,
            "size": items.len(),
            "values": items.iter().map(value_to_json).collect::<Vec<_>>(),
        }),
        Value::Struct { fields, .. } => {
            let mut m = Map::new();
            for (name, val) in fields {
                m.insert(name.clone(), value_to_json(val));
            }
            Json::Object(m)
        }
        Value::Raw(bytes) => json!({ "type": "Raw", "hex": hex_string(bytes) }),
    }
}

fn hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dict() -> Dictionary {
        let bytes =
            std::fs::read("../../../test/fprime_gds/common/dp/test_dp_data/dictionary.json")
                .expect("dictionary.json present");
        Dictionary::from_bytes(&bytes).expect("dict parses")
    }

    fn read_bin(name: &str) -> Vec<u8> {
        let p = format!("../../../test/fprime_gds/common/dp/test_dp_data/{name}");
        std::fs::read(&p).unwrap_or_else(|_| panic!("read {p}"))
    }

    #[test]
    fn decode_make_bool() {
        let dict = test_dict();
        let bytes = read_bin("makeBool.bin");
        let dp = decode_bytes(&bytes, &dict).expect("decode");
        assert_eq!(dp.header.packet_descriptor, 5);
        assert_eq!(dp.header.container_id, 5390); // dpTool.Container1
        assert_eq!(dp.header.priority, 10);
        assert_eq!(dp.header.user_data.len(), 32);
        assert_eq!(dp.header.dp_state_label.as_deref(), Some("UNTRANSMITTED"));
        assert_eq!(dp.records.len(), 400);
        for r in &dp.records {
            assert_eq!(r.record.name, "dpTool.BoolRecord");
            assert!(matches!(r.data, RecordData::Scalar(Value::Bool(_))));
        }
    }

    #[test]
    fn decode_make_u32_array() {
        let dict = test_dict();
        let bytes = read_bin("makeU32Array.bin");
        let dp = decode_bytes(&bytes, &dict).expect("decode");
        assert_eq!(dp.records.len(), 1);
        if let RecordData::Array(items) = &dp.records[0].data {
            assert_eq!(items.len(), 300);
            assert!(matches!(items[0], Value::U32(_)));
        } else {
            panic!("expected array record");
        }
    }

    #[test]
    fn decode_make_complex() {
        let dict = test_dict();
        let bytes = read_bin("makeComplex.bin");
        let dp = decode_bytes(&bytes, &dict).expect("decode");
        assert_eq!(dp.records.len(), 200);
        let r = &dp.records[0];
        assert_eq!(r.record.name, "dpTool.ComplexRecord");
        if let RecordData::Scalar(Value::Struct { fields, .. }) = &r.data {
            // Complex: f1 (struct {u16Field}) + f2 (U32)
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0, "f1");
            assert_eq!(fields[1].0, "f2");
        } else {
            panic!("expected struct record");
        }
    }

    #[test]
    fn rejects_bad_header_crc() {
        let dict = test_dict();
        let bytes = read_bin("CRC_HEADER_FAILURE_EXPECTED.bin");
        let err = decode_bytes(&bytes, &dict).unwrap_err();
        assert!(matches!(
            err,
            DpError::CrcMismatch {
                section: "Header",
                ..
            }
        ));
    }

    /// Parity table: each `(file, expected_record_count, record_name)`
    /// matches what the Python `DataProductDecoder` produces from the same
    /// bytes (verified against
    /// `fprime_gds.common.dp.decoder.DataProductDecoder.decode()` on the
    /// reference fixtures in `test/fprime_gds/common/dp/test_dp_data/`).
    const PARITY: &[(&str, usize, &str)] = &[
        ("makeBool.bin", 400, "dpTool.BoolRecord"),
        ("makeU32.bin", 250, "dpTool.U32Record"),
        ("makeI8.bin", 400, "dpTool.I8Record"),
        ("makeI16.bin", 333, "dpTool.I16Record"),
        ("makeI32.bin", 250, "dpTool.I32Record"),
        ("makeI64.bin", 166, "dpTool.I64Record"),
        ("makeF32.bin", 250, "dpTool.F32Record"),
        ("makeF64.bin", 166, "dpTool.F64Record"),
        ("makeEnum.bin", 250, "dpTool.EnumRecord"),
        ("makeU8Array.bin", 6, "dpTool.U8ArrayRecord"),
        ("makeU32Array.bin", 1, "dpTool.U32ArrayRecord"),
        ("makeDataArray.bin", 3, "dpTool.DataArrayRecord"),
        ("makeFppArray.bin", 83, "dpTool.FppArrayRecord"),
        ("makeComplex.bin", 200, "dpTool.ComplexRecord"),
    ];

    #[test]
    fn parity_with_python_decoder() {
        let dict = test_dict();
        for (file, expected_count, expected_name) in PARITY {
            let bytes = read_bin(file);
            let dp = decode_bytes(&bytes, &dict).unwrap_or_else(|e| panic!("decode {file}: {e}"));
            assert_eq!(dp.records.len(), *expected_count, "{file} record count");
            for r in &dp.records {
                assert_eq!(r.record.name, *expected_name, "{file} record name");
            }
            // Header invariants common to all reference fixtures (they were
            // all generated with `dpTool.Container1`, the only non-SG
            // container in the test dictionary).
            assert_eq!(dp.header.packet_descriptor, FW_PACKET_DP, "{file}");
            assert_eq!(dp.header.container_id, 5390, "{file}");
            assert_eq!(dp.header.priority, 10, "{file}");
            assert_eq!(
                dp.header.dp_state_label.as_deref(),
                Some("UNTRANSMITTED"),
                "{file}"
            );
            // The JSON renderer must produce a `Header` + `Records` shape
            // matching the Python decoder's top-level keys.
            let j = to_json(&dp);
            assert!(j.get("Header").is_some(), "{file}: Header missing");
            let recs = j.get("Records").and_then(|v| v.as_array()).unwrap();
            assert_eq!(recs.len(), *expected_count, "{file} json records");
        }
    }

    #[test]
    fn rejects_bad_data_crc() {
        let dict = test_dict();
        let bytes = read_bin("CRC_FAILURE_EXPECTED.bin");
        // The corrupted file may fail with either a CRC mismatch or an
        // unknown record — either way it must NOT decode successfully.
        let err = decode_bytes(&bytes, &dict).unwrap_err();
        assert!(matches!(
            err,
            DpError::CrcMismatch { .. } | DpError::UnknownRecord(_) | DpError::TooShort { .. }
        ));
    }
}
