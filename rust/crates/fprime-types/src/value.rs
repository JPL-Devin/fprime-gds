//! Dynamic value type used by the dictionary-driven decoder/encoder pipeline.
//!
//! Each [`Value`] variant corresponds to a primitive F´ type.  The dictionary
//! describes which type a given event/channel/command argument has, and
//! [`Value`] is the runtime carrier for that data.

use std::fmt;

use crate::{FpString, Serde, TypeError};

/// A dynamic F´ value.  Primitive variants carry the value directly; the
/// compound variants (`Enum`, `Array`, `Struct`) are filled in by the
/// dictionary-driven deserializer in `fprime-pipeline`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    String(FpString),
    /// FPP `enum` value.  `label` is the constant name when the wire value
    /// matched a declared constant, `None` otherwise.
    Enum {
        type_name: String,
        label: Option<String>,
        value: i64,
    },
    /// FPP `array` of any other type.
    Array {
        type_name: String,
        items: Vec<Value>,
    },
    /// FPP `struct`, members in wire order.
    Struct {
        type_name: String,
        fields: Vec<(String, Value)>,
    },
    /// Catch-all for bytes the decoder couldn't classify.
    Raw(Vec<u8>),
}

impl Value {
    /// Total wire size in bytes.  Returns `None` for variable-length values
    /// (`String`, `Raw`, `Array`, `Struct`).
    pub fn fixed_size(&self) -> Option<usize> {
        Some(match self {
            Value::Bool(_) => 1,
            Value::U8(_) | Value::I8(_) => 1,
            Value::U16(_) | Value::I16(_) => 2,
            Value::U32(_) | Value::I32(_) | Value::F32(_) => 4,
            Value::U64(_) | Value::I64(_) | Value::F64(_) => 8,
            Value::String(_)
            | Value::Raw(_)
            | Value::Enum { .. }
            | Value::Array { .. }
            | Value::Struct { .. } => return None,
        })
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::U8(_) => "U8",
            Value::U16(_) => "U16",
            Value::U32(_) => "U32",
            Value::U64(_) => "U64",
            Value::I8(_) => "I8",
            Value::I16(_) => "I16",
            Value::I32(_) => "I32",
            Value::I64(_) => "I64",
            Value::F32(_) => "F32",
            Value::F64(_) => "F64",
            Value::String(_) => "string",
            Value::Enum { .. } => "enum",
            Value::Array { .. } => "array",
            Value::Struct { .. } => "struct",
            Value::Raw(_) => "raw",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(v) => write!(f, "{v}"),
            Value::U8(v) => write!(f, "{v}"),
            Value::U16(v) => write!(f, "{v}"),
            Value::U32(v) => write!(f, "{v}"),
            Value::U64(v) => write!(f, "{v}"),
            Value::I8(v) => write!(f, "{v}"),
            Value::I16(v) => write!(f, "{v}"),
            Value::I32(v) => write!(f, "{v}"),
            Value::I64(v) => write!(f, "{v}"),
            Value::F32(v) => write!(f, "{v}"),
            Value::F64(v) => write!(f, "{v}"),
            Value::String(s) => write!(f, "{:?}", s.0),
            Value::Enum {
                label,
                value,
                type_name: _,
            } => match label {
                Some(name) => write!(f, "{name}({value})"),
                None => write!(f, "<unknown>({value})"),
            },
            Value::Array { items, .. } => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Struct { fields, .. } => {
                write!(f, "{{")?;
                for (i, (name, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: {value}")?;
                }
                write!(f, "}}")
            }
            Value::Raw(bytes) => {
                write!(f, "0x")?;
                for b in bytes {
                    write!(f, "{:02x}", b)?;
                }
                Ok(())
            }
        }
    }
}

impl Value {
    /// Serialize a value to F´ wire format.
    pub fn serialize(&self, out: &mut Vec<u8>) {
        match self {
            Value::Bool(v) => v.serialize(out),
            Value::U8(v) => v.serialize(out),
            Value::U16(v) => v.serialize(out),
            Value::U32(v) => v.serialize(out),
            Value::U64(v) => v.serialize(out),
            Value::I8(v) => v.serialize(out),
            Value::I16(v) => v.serialize(out),
            Value::I32(v) => v.serialize(out),
            Value::I64(v) => v.serialize(out),
            Value::F32(v) => v.serialize(out),
            Value::F64(v) => v.serialize(out),
            Value::String(s) => s.serialize(out),
            Value::Enum { value, .. } => {
                // Best-effort: serialise the enum's underlying integer as I64.
                // The dictionary-driven encoder in `fprime-pipeline` should
                // build enum values with the correct width before calling
                // this; we only hit this path if the user constructs an
                // `Enum` value directly.
                value.serialize(out);
            }
            Value::Array { items, .. } => {
                for item in items {
                    item.serialize(out);
                }
            }
            Value::Struct { fields, .. } => {
                for (_, value) in fields {
                    value.serialize(out);
                }
            }
            Value::Raw(bytes) => out.extend_from_slice(bytes),
        }
    }

    /// Deserialize a value of the given primitive type name.  Returns
    /// `(value, bytes_consumed)`.
    pub fn deserialize_named(
        type_name: &str,
        data: &[u8],
        offset: usize,
    ) -> Result<(Value, usize), TypeError> {
        match type_name {
            "bool" => bool::deserialize(data, offset).map(|(v, n)| (Value::Bool(v), n)),
            "U8" => u8::deserialize(data, offset).map(|(v, n)| (Value::U8(v), n)),
            "U16" => u16::deserialize(data, offset).map(|(v, n)| (Value::U16(v), n)),
            "U32" => u32::deserialize(data, offset).map(|(v, n)| (Value::U32(v), n)),
            "U64" => u64::deserialize(data, offset).map(|(v, n)| (Value::U64(v), n)),
            "I8" => i8::deserialize(data, offset).map(|(v, n)| (Value::I8(v), n)),
            "I16" => i16::deserialize(data, offset).map(|(v, n)| (Value::I16(v), n)),
            "I32" => i32::deserialize(data, offset).map(|(v, n)| (Value::I32(v), n)),
            "I64" => i64::deserialize(data, offset).map(|(v, n)| (Value::I64(v), n)),
            "F32" => f32::deserialize(data, offset).map(|(v, n)| (Value::F32(v), n)),
            "F64" => f64::deserialize(data, offset).map(|(v, n)| (Value::F64(v), n)),
            "string" => FpString::deserialize(data, offset).map(|(v, n)| (Value::String(v), n)),
            other => Err(TypeError::UnknownType(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_u32() {
        let v = Value::U32(0xDEADBEEF);
        let mut buf = Vec::new();
        v.serialize(&mut buf);
        let (r, n) = Value::deserialize_named("U32", &buf, 0).unwrap();
        assert_eq!(n, 4);
        assert_eq!(r, v);
    }

    #[test]
    fn round_trip_string() {
        let v = Value::String(FpString("hello".to_string()));
        let mut buf = Vec::new();
        v.serialize(&mut buf);
        let (r, n) = Value::deserialize_named("string", &buf, 0).unwrap();
        assert_eq!(n, 2 + 5);
        assert_eq!(r, v);
    }
}
