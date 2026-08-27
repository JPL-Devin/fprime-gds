//! F Prime type model: deserialization and JSON rendering matching the Python
//! `fprime_gds.common.models.serialize` type classes.

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntKind {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntKind {
    pub fn from_name(name: &str) -> Option<IntKind> {
        Some(match name {
            "I8" => IntKind::I8,
            "I16" => IntKind::I16,
            "I32" => IntKind::I32,
            "I64" => IntKind::I64,
            "U8" => IntKind::U8,
            "U16" => IntKind::U16,
            "U32" => IntKind::U32,
            "U64" => IntKind::U64,
            _ => return None,
        })
    }

    pub fn size(&self) -> usize {
        match self {
            IntKind::I8 | IntKind::U8 => 1,
            IntKind::I16 | IntKind::U16 => 2,
            IntKind::I32 | IntKind::U32 => 4,
            IntKind::I64 | IntKind::U64 => 8,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            IntKind::I8 => "I8",
            IntKind::I16 => "I16",
            IntKind::I32 => "I32",
            IntKind::I64 => "I64",
            IntKind::U8 => "U8",
            IntKind::U16 => "U16",
            IntKind::U32 => "U32",
            IntKind::U64 => "U64",
        }
    }

    pub fn read(&self, buf: &[u8], offset: &mut usize) -> Result<Value, String> {
        let size = self.size();
        let bytes = take(buf, offset, size)?;
        let value = match self {
            IntKind::I8 => json!(bytes[0] as i8),
            IntKind::I16 => json!(i16::from_be_bytes(bytes.try_into().unwrap())),
            IntKind::I32 => json!(i32::from_be_bytes(bytes.try_into().unwrap())),
            IntKind::I64 => json!(i64::from_be_bytes(bytes.try_into().unwrap())),
            IntKind::U8 => json!(bytes[0]),
            IntKind::U16 => json!(u16::from_be_bytes(bytes.try_into().unwrap())),
            IntKind::U32 => json!(u32::from_be_bytes(bytes.try_into().unwrap())),
            IntKind::U64 => json!(u64::from_be_bytes(bytes.try_into().unwrap())),
        };
        Ok(value)
    }
}

/// A struct member: (name, type, format, description)
pub type StructMember = (String, FType, String, String);

/// The F Prime type model, mirroring the Python DictionaryType classes
#[derive(Debug, Clone)]
pub enum FType {
    Int(IntKind),
    F32,
    F64,
    Bool,
    /// max length; class name is `String_{max}`
    String(usize),
    /// (qualified name, representation type, [(key, value)])
    Enum(String, IntKind, Vec<(String, i64)>),
    /// (class name, element type, length, format)
    Array(String, Box<FType>, usize, String),
    /// (qualified name, members)
    Struct(String, Vec<StructMember>),
    /// Fw.Time: U16 base, U8 context, U32 seconds, U32 microseconds
    Time,
}

pub fn take<'a>(buf: &'a [u8], offset: &mut usize, size: usize) -> Result<&'a [u8], String> {
    if *offset + size > buf.len() {
        return Err(format!(
            "Not enough data to deserialize. Needed: {} Left: {}",
            size,
            buf.len() - *offset
        ));
    }
    let out = &buf[*offset..*offset + size];
    *offset += size;
    Ok(out)
}

impl FType {
    /// Equivalent of Python's `repr(instance)`: class name with "Type" removed
    pub fn repr(&self) -> String {
        self.class_name().replace("Type", "")
    }

    /// Equivalent of the Python class `__name__`
    pub fn class_name(&self) -> String {
        match self {
            FType::Int(kind) => format!("{}Type", kind.name()),
            FType::F32 => "F32Type".to_string(),
            FType::F64 => "F64Type".to_string(),
            FType::Bool => "BoolType".to_string(),
            FType::String(max) => format!("String_{}", max),
            FType::Enum(name, _, _) => name.clone(),
            FType::Array(name, _, _, _) => name.clone(),
            FType::Struct(name, _) => name.clone(),
            FType::Time => "TimeType".to_string(),
        }
    }

    /// Equivalent of Python's `str(cls)` e.g. `<class 'abc.Svc.DpTool.Complex'>`
    pub fn class_str(&self) -> String {
        match self {
            FType::Int(_) | FType::F32 | FType::F64 => format!(
                "<class 'fprime_gds.common.models.serialize.numerical_types.{}'>",
                self.class_name()
            ),
            FType::Bool => {
                "<class 'fprime_gds.common.models.serialize.bool_type.BoolType'>".to_string()
            }
            FType::Time => {
                "<class 'fprime_gds.common.models.serialize.time_type.TimeType'>".to_string()
            }
            _ => format!("<class 'abc.{}'>", self.class_name()),
        }
    }

    /// Maximum serialized size, equivalent of Python `getMaxSize()`
    pub fn max_size(&self, size_store_len: usize) -> usize {
        match self {
            FType::Int(kind) => kind.size(),
            FType::F32 => 4,
            FType::F64 => 8,
            FType::Bool => 1,
            FType::String(max) => size_store_len + max,
            FType::Enum(_, rep, _) => rep.size(),
            FType::Array(_, elem, len, _) => elem.max_size(size_store_len) * len,
            FType::Struct(_, members) => members
                .iter()
                .map(|(_, t, _, _)| t.max_size(size_store_len))
                .sum(),
            FType::Time => 11,
        }
    }

    /// Deserialize from a buffer, producing the Python `.val` equivalent
    /// representation (numbers, strings for enums, dicts for structs, lists
    /// for arrays). `size_store_len` is the size of FwSizeStoreType, used
    /// for string length prefixes.
    pub fn deserialize(
        &self,
        buf: &[u8],
        offset: &mut usize,
        size_store_len: usize,
    ) -> Result<Value, String> {
        match self {
            FType::Int(kind) => kind.read(buf, offset),
            FType::F32 => {
                let bytes = take(buf, offset, 4)?;
                Ok(json!(f32::from_be_bytes(bytes.try_into().unwrap()) as f64))
            }
            FType::F64 => {
                let bytes = take(buf, offset, 8)?;
                Ok(json!(f64::from_be_bytes(bytes.try_into().unwrap())))
            }
            FType::Bool => {
                let bytes = take(buf, offset, 1)?;
                Ok(json!(bytes[0] != 0))
            }
            FType::String(_) => {
                let len_bytes = take(buf, offset, size_store_len)?;
                let mut len: usize = 0;
                for b in len_bytes {
                    len = (len << 8) | (*b as usize);
                }
                let str_bytes = take(buf, offset, len)?;
                Ok(json!(String::from_utf8_lossy(str_bytes).to_string()))
            }
            FType::Enum(name, rep, members) => {
                let raw = rep.read(buf, offset)?;
                let int_val = raw.as_i64().or_else(|| raw.as_u64().map(|v| v as i64));
                let int_val =
                    int_val.ok_or_else(|| format!("Bad enum representation for {}", name))?;
                for (key, val) in members {
                    if *val == int_val {
                        return Ok(json!(key));
                    }
                }
                Err(format!(
                    "Invalid enumeration value {} for enum {}",
                    int_val, name
                ))
            }
            FType::Array(_, elem, len, _) => {
                let mut values = Vec::with_capacity(*len);
                for _ in 0..*len {
                    values.push(elem.deserialize(buf, offset, size_store_len)?);
                }
                Ok(Value::Array(values))
            }
            FType::Struct(_, members) => {
                let mut map = Map::new();
                for (name, ftype, _, _) in members {
                    map.insert(
                        name.clone(),
                        ftype.deserialize(buf, offset, size_store_len)?,
                    );
                }
                Ok(Value::Object(map))
            }
            FType::Time => {
                let base = IntKind::U16.read(buf, offset)?;
                let context = IntKind::U8.read(buf, offset)?;
                let seconds = IntKind::U32.read(buf, offset)?;
                let useconds = IntKind::U32.read(buf, offset)?;
                Ok(json!({
                    "base": base,
                    "context": context,
                    "seconds": seconds,
                    "microseconds": useconds,
                }))
            }
        }
    }

    /// Render a deserialized value as the Python `to_jsonable()` equivalent
    pub fn jsonable(&self, val: &Value) -> Value {
        match self {
            FType::Int(_) | FType::F32 | FType::F64 | FType::Bool | FType::String(_) => {
                json!({"value": val, "type": self.repr()})
            }
            FType::Enum(_, _, _) => json!({"value": val, "type": self.repr()}),
            FType::Array(name, elem, len, format) => {
                json!({
                    "name": name,
                    "type": name,
                    "size": len,
                    "format": format,
                    "value_type": elem.repr(),
                    "values": val,
                })
            }
            FType::Struct(_, members) => {
                let mut map = Map::new();
                for (name, ftype, format, description) in members {
                    let mut entry = Map::new();
                    entry.insert("format".to_string(), json!(format));
                    entry.insert("description".to_string(), json!(description));
                    let inner = ftype.jsonable(&val[name]);
                    if let Value::Object(inner_map) = inner {
                        for (k, v) in inner_map {
                            entry.insert(k, v);
                        }
                    }
                    map.insert(name.clone(), Value::Object(entry));
                }
                Value::Object(map)
            }
            FType::Time => {
                let mut map = Map::new();
                map.insert("type".to_string(), json!("Time"));
                for key in ["base", "context", "seconds", "microseconds"] {
                    map.insert(key.to_string(), val[key].clone());
                }
                Value::Object(map)
            }
        }
    }
}
