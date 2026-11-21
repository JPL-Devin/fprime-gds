//! Reading a `.dat` file: the checks and messages of `fprime-prm-decode`.

use crate::{checksum, Param, ParamDictionary, DELIMITER, ID_SIZE};
use fgds_core::dictionary::Type;
use sequences::convert::type_name;
use sequences::value::Value;

/// `len` bytes at `offset`, or the message of Python's `struct.unpack_from`.
fn take(data: &[u8], offset: usize, len: usize) -> Result<&[u8], String> {
    offset.checked_add(len).and_then(|end| data.get(offset..end)).ok_or_else(|| {
        format!(
            "unpack_from requires a buffer of at least {} bytes for unpacking {len} bytes at offset {offset} (actual buffer size is {})",
            offset.saturating_add(len),
            data.len()
        )
    })
}

fn int(bytes: &[u8], signed: bool) -> i128 {
    let unsigned = bytes.iter().fold(0i128, |acc, b| (acc << 8) | i128::from(*b));
    let bits = bytes.len() * 8;
    match signed && bits > 0 && unsigned >> (bits - 1) == 1 {
        true => unsigned - (1i128 << bits),
        false => unsigned,
    }
}

fn number(ty: &Type, bytes: &[u8]) -> Value {
    match (ty, bytes) {
        (Type::Float { .. }, [a, b, c, d]) => Value::Float(f64::from(f32::from_be_bytes([*a, *b, *c, *d]))),
        (Type::Float { .. }, _) => {
            let mut wide = [0u8; 8];
            wide.iter_mut().zip(bytes).for_each(|(w, b)| *w = *b);
            Value::Float(f64::from_be_bytes(wide))
        }
        (Type::Int { signed, .. }, _) => Value::Int(int(bytes, *signed).into()),
        _ => Value::Null,
    }
}

fn width(ty: &Type) -> usize {
    match ty {
        Type::Int { bits, .. } | Type::Float { bits } => usize::from(*bits / 8),
        _ => 0,
    }
}

fn array(
    dict: &ParamDictionary,
    element: &Type,
    size: usize,
    data: &[u8],
    offset: &mut usize,
) -> Result<Value, String> {
    let mut items = Vec::new();
    if matches!(element, Type::Int { .. } | Type::Float { .. }) {
        let each = width(element);
        let bytes = take(data, *offset, each.saturating_mul(size))
            .map_err(|e| format!("Array NumericalType optimization failed to deserialize: {e}"))?;
        items.extend(bytes.chunks(each.max(1)).map(|chunk| number(element, chunk)));
        *offset += bytes.len();
    } else {
        for index in 0..size {
            // The count comes from the dictionary: stop at the first element the data lacks.
            let item = value(dict, element, data, offset)
                .map_err(|e| format!("Array index {index} failed to deserialize: {e}"))?;
            items.push(item);
        }
    }
    Ok(Value::Array(items))
}

/// One value at `offset`, which moves past it.
fn value(dict: &ParamDictionary, ty: &Type, data: &[u8], offset: &mut usize) -> Result<Value, String> {
    let left = data.len().saturating_sub(*offset);
    match ty {
        Type::Int { .. } | Type::Float { .. } => {
            let bytes = take(data, *offset, width(ty))?;
            *offset += bytes.len();
            Ok(number(ty, bytes))
        }
        Type::Bool => {
            let byte = *data.get(*offset).ok_or("Not enough bytes to deserialize bool.")?;
            *offset += 1;
            let truth = |b: bool| {
                let mut out = Vec::new();
                let _ = dict.types.encode(&Type::Bool, &b.into(), &mut out);
                out.first().copied()
            };
            match (Some(byte) == truth(true), Some(byte) == truth(false)) {
                (true, _) => Ok(Value::Bool(true)),
                (_, true) => Ok(Value::Bool(false)),
                _ => Err(format!("Value {byte} out of range!")),
            }
        }
        Type::String { max } => {
            let prefix = width(&dict.types.config_type("FwSizeStoreType", Type::U16)).max(1);
            let Ok(size) = take(data, *offset, prefix) else {
                return Err("Not enough bytes to deserialize string length.".into());
            };
            let size = int(size, false) as usize;
            let left = left - prefix;
            if left < size {
                return Err(format!("Not enough data to deserialize string data. Needed: {size} Left: {left}"));
            }
            if size > *max {
                return Err(format!("String size {size} is greater than {max}!"));
            }
            let start = *offset + prefix;
            let text = std::str::from_utf8(&data[start..start + size]).map_err(|e| {
                format!("'utf-8' codec can't decode the string: invalid byte at position {}", e.valid_up_to())
            })?;
            *offset = start + size;
            Ok(Value::Str(text.to_string()))
        }
        Type::Enum { rep, constants, .. } => {
            let size = width(rep);
            let Ok(bytes) = take(data, *offset, size) else {
                return Err(format!("Could not deserialize enum value. Needed: {size} bytes Found: {left}"));
            };
            *offset += size;
            let number = int(bytes, matches!(**rep, Type::Int { signed: true, .. }));
            match constants.iter().find(|(_, v)| i128::from(*v) == number) {
                Some((name, _)) => Ok(Value::Str(name.clone())),
                None => Err(format!("Value {number} out of range!")),
            }
        }
        Type::Array { size, element, .. } => array(dict, element, *size, data, offset),
        Type::Struct { members, .. } => {
            let mut pairs = Vec::new();
            for member in members {
                let item = match member.count {
                    Some(count) => array(dict, &member.ty, count, data, offset),
                    None => value(dict, &member.ty, data, offset),
                };
                let shown = match member.count {
                    Some(count) => format!("Array_{}_{count}", type_name(&member.ty)),
                    None => type_name(&member.ty),
                };
                let item = item.map_err(|e| format!("Field '{}' [{shown}] failed to deserialize: {e}", member.name))?;
                pairs.push((member.name.clone(), item));
            }
            Ok(Value::Object(pairs))
        }
    }
}

fn be32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |acc, b| (acc << 8) | u32::from(*b))
}

pub(crate) fn from_dat<'a>(dict: &'a ParamDictionary, dat: &[u8]) -> Result<Vec<(&'a Param, Value)>, String> {
    if dat.len() < 4 {
        return Err(format!("File too small to contain CRC header: expected at least 4 bytes, got {}", dat.len()));
    }
    let (file_crc, data) = (be32(&dat[..4]), &dat[4..]);
    let computed = checksum(data);
    if file_crc != computed {
        return Err(format!("CRC mismatch: file header has 0x{file_crc:08x}, computed 0x{computed:08x}"));
    }
    let mut params = Vec::new();
    // Offsets in messages count from the start of the file.
    let mut offset = 0;
    while offset < data.len() {
        if data[offset] != DELIMITER {
            let (at, found) = (offset + 4, data[offset]);
            return Err(format!("Invalid delimiter at offset {at}: expected 0xA5, got {found:#x}"));
        }
        offset += 1;
        let field = |offset: usize, what: &str| {
            data.get(offset..offset + 4).map(be32).ok_or_else(|| {
                format!("Incomplete {what} at offset {}: expected 4 bytes, got {}", offset + 4, data.len() - offset)
            })
        };
        let record_size = field(offset, "record size")? as usize;
        offset += 4;
        let id = field(offset, "parameter ID")?;
        offset += 4;
        // Among parameters with the same identifier the Python tool keeps the last.
        let Some(param) = dict.params.iter().rev().find(|p| p.id == id) else {
            return Err(format!("Unknown parameter ID {id} (0x{id:x}) at offset {}", offset - 4));
        };
        let name = param.full_name();
        let Some(value_size) = record_size.checked_sub(ID_SIZE) else {
            return Err(format!(
                "Record of {name} at offset {}: size {record_size} is less than the identifier",
                offset - 4
            ));
        };
        let left = data.len() - offset;
        if value_size > left {
            let at = offset + 4;
            return Err(format!(
                "Incomplete parameter value for {name} at offset {at}: expected {value_size} bytes, got {left}"
            ));
        }
        let mut end = offset;
        let decoded = value(dict, &param.ty, data, &mut end).map_err(|e| {
            format!("Failed to deserialize parameter {name} (id={id}, type={}): {e}", type_name(&param.ty))
        })?;
        params.push((param, decoded));
        offset += value_size;
    }
    Ok(params)
}
