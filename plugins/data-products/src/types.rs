//! The types of a data product, built from the dictionary JSON the way the Python GDS
//! builds its type classes: same names, same sizes, same decoding errors, same
//! `to_jsonable` output. `fgds-dictionary` does not keep member formats and
//! descriptions, which the decoded output shows, so the dictionary is read here.

use crate::json::Json;
use fgds_core::serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Ty {
    Int { bits: u8, signed: bool },
    Float { bits: u8 },
    Bool,
    Str { max: u64 },
    Enum(Arc<EnumTy>),
    Array(Arc<ArrayTy>),
    Struct(Arc<StructTy>),
    Time(Arc<TimeTy>),
}

#[derive(Debug)]
pub struct EnumTy {
    pub name: String,
    pub bits: u8,
    pub signed: bool,
    pub constants: Vec<(String, i128)>,
}

#[derive(Debug)]
pub struct ArrayTy {
    pub name: String,
    pub len: u64,
    pub format: String,
    pub element: Ty,
}

#[derive(Debug)]
pub struct StructTy {
    pub name: String,
    pub members: Vec<Member>,
}

#[derive(Debug)]
pub struct Member {
    pub name: String,
    pub ty: Ty,
    pub format: String,
    pub description: String,
}

#[derive(Debug)]
pub struct TimeTy {
    pub base: Ty,
    pub context: Ty,
}

/// A decoded value; its type says how it is written.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    /// Constant name and its number.
    Enum(String, i128),
    List(Vec<Val>),
    /// Member values in member order.
    Struct(Vec<Val>),
    Time([Box<Val>; 4]),
}

impl Val {
    pub fn int(&self) -> Option<i128> {
        match self {
            Val::Int(i) => Some(*i),
            _ => None,
        }
    }
}

const U32: Ty = Ty::Int { bits: 32, signed: false };
const U16: Ty = Ty::Int { bits: 16, signed: false };
const U8: Ty = Ty::Int { bits: 8, signed: false };

/// Arrays of zero-size elements are refused above this length (LNK-9): they cost
/// memory without consuming input.
const MAX_EMPTY_ELEMENTS: u64 = 65_536;

fn primitive(name: &str) -> Option<Ty> {
    let int = |bits, signed| Some(Ty::Int { bits, signed });
    match name {
        "U8" => int(8, false),
        "U16" => int(16, false),
        "U32" => int(32, false),
        "U64" => int(64, false),
        "I8" => int(8, true),
        "I16" => int(16, true),
        "I32" => int(32, true),
        "I64" => int(64, true),
        "F32" => Some(Ty::Float { bits: 32 }),
        "F64" => Some(Ty::Float { bits: 64 }),
        "bool" => Some(Ty::Bool),
        _ => None,
    }
}

impl Ty {
    /// The name of the Python class of this type.
    pub fn class_name(&self) -> String {
        match self {
            Ty::Int { bits, signed } => format!("{}{bits}Type", if *signed { "I" } else { "U" }),
            Ty::Float { bits } => format!("F{bits}Type"),
            Ty::Bool => "BoolType".into(),
            Ty::Str { max } => format!("String_{max}"),
            Ty::Enum(e) => e.name.clone(),
            Ty::Array(a) => a.name.clone(),
            Ty::Struct(s) => s.name.clone(),
            Ty::Time(_) => "TimeType".into(),
        }
    }

    /// `repr()` of an instance: the class name without every "Type".
    pub fn repr(&self) -> String {
        self.class_name().replace("Type", "")
    }

    /// `str()` of the Python class, which `fprime-dp` writes as `record_type`.
    pub fn class_text(&self) -> String {
        let module = match self {
            Ty::Int { .. } | Ty::Float { .. } => "fprime_gds.common.models.serialize.numerical_types",
            Ty::Bool => "fprime_gds.common.models.serialize.bool_type",
            Ty::Time(_) => "fprime_gds.common.models.serialize.time_type",
            // Classes built from the dictionary are created through an abstract base class.
            _ => "abc",
        };
        format!("<class '{module}.{}'>", self.class_name())
    }

    fn is_numerical(&self) -> bool {
        matches!(self, Ty::Int { .. } | Ty::Float { .. })
    }
}

/// The error text of `struct.unpack_from` when `need` bytes are not there.
fn unpack_error(data: &[u8], offset: usize, need: u64) -> String {
    if offset > data.len() {
        return format!("offset {offset} out of range for {}-byte buffer", data.len());
    }
    format!(
        "unpack_from requires a buffer of at least {} bytes for unpacking {need} bytes at offset {offset} \
         (actual buffer size is {})",
        (offset as u64).saturating_add(need),
        data.len()
    )
}

fn bytes_at(data: &[u8], offset: usize, need: usize) -> Result<&[u8], String> {
    match offset.checked_add(need).and_then(|end| data.get(offset..end)) {
        Some(bytes) => Ok(bytes),
        None => Err(unpack_error(data, offset, need as u64)),
    }
}

fn number(bytes: &[u8], ty: &Ty) -> Val {
    let unsigned = bytes.iter().fold(0u64, |acc, b| acc << 8 | u64::from(*b));
    match ty {
        Ty::Float { bits: 32 } => Val::Float(f64::from(f32::from_bits(unsigned as u32))),
        Ty::Float { .. } => Val::Float(f64::from_bits(unsigned)),
        Ty::Int { signed: true, .. } => {
            let shift = 64 - 8 * bytes.len() as u32;
            Val::Int(i128::from((unsigned << shift) as i64 >> shift))
        }
        _ => Val::Int(i128::from(unsigned)),
    }
}

/// The message of Python's `UnicodeDecodeError` for the `utf-8` codec.
fn utf8_error(bytes: &[u8], e: std::str::Utf8Error) -> String {
    let at = e.valid_up_to();
    let byte = bytes[at];
    let reason = match e.error_len() {
        None => "unexpected end of data",
        Some(_) if !(0xc2..=0xf4).contains(&byte) => "invalid start byte",
        Some(_) => "invalid continuation byte",
    };
    format!("'utf-8' codec can't decode byte {byte:#04x} in position {at}: {reason}")
}

/// One record definition of the dictionary.
#[derive(Debug)]
pub struct RecordDef {
    pub id: i128,
    pub name: String,
    pub ty: Ty,
    pub is_array: bool,
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct ContainerDef {
    pub id: i128,
    pub name: String,
    pub default_priority: i128,
    pub description: Option<String>,
}

/// What the data product tools need from a dictionary.
#[derive(Debug)]
pub struct DpDictionary {
    /// Type definitions by qualified name; an unusable definition keeps its error.
    types: HashMap<String, Result<Ty, String>>,
    constants: HashMap<String, i128>,
    pub records: HashMap<i128, RecordDef>,
    pub containers: HashMap<i128, ContainerDef>,
}

struct Builder<'a> {
    definitions: HashMap<&'a str, &'a Value>,
    done: HashMap<String, Result<Ty, String>>,
    active: HashSet<String>,
}

impl Builder<'_> {
    /// A reference to a type: a primitive, a string, or a named definition.
    fn reference(&mut self, reference: &Value) -> Result<Ty, String> {
        let name =
            reference["name"].as_str().ok_or_else(|| format!("Dictionary entry has no `name` field: {reference}"))?;
        if let Some(ty) = primitive(name) {
            return Ok(ty);
        }
        if name == "string" {
            let max = reference["size"].as_u64().ok_or_else(|| format!("string type has no size: {reference}"))?;
            return Ok(Ty::Str { max });
        }
        if let Some(done) = self.done.get(name) {
            return done.clone();
        }
        let Some(definition) = self.definitions.get(name).copied() else {
            return Err(format!("Dictionary type name has no corresponding type definition: {name}"));
        };
        if !self.active.insert(name.to_string()) {
            return Err(format!("type {name} contains itself"));
        }
        let ty = self.definition(name, definition);
        self.active.remove(name);
        self.done.insert(name.to_string(), ty.clone());
        ty
    }

    fn definition(&mut self, name: &str, def: &Value) -> Result<Ty, String> {
        let field = |key: &str| format!("type definition {name} has no usable `{key}`");
        match def["kind"].as_str() {
            Some("alias") => self.reference(&def["underlyingType"]),
            Some("array") => Ok(Ty::Array(Arc::new(ArrayTy {
                name: name.to_string(),
                len: def["size"].as_u64().ok_or_else(|| field("size"))?,
                format: format_string(&def["elementType"]["format"]),
                element: self.reference(&def["elementType"])?,
            }))),
            Some("enum") => {
                let rep = def["representationType"]["name"].as_str().and_then(primitive);
                let Some(Ty::Int { bits, signed }) = rep else { return Err(field("representationType")) };
                let mut constants: Vec<(String, i128)> = Vec::new();
                for constant in def["enumeratedConstants"].as_array().ok_or_else(|| field("enumeratedConstants"))? {
                    let key = constant["name"].as_str().ok_or_else(|| field("enumeratedConstants"))?;
                    let value = integer(&constant["value"]).ok_or_else(|| field("enumeratedConstants"))?;
                    match constants.iter_mut().find(|(k, _)| k == key) {
                        Some(entry) => entry.1 = value,
                        None => constants.push((key.to_string(), value)),
                    }
                }
                Ok(Ty::Enum(Arc::new(EnumTy { name: name.to_string(), bits, signed, constants })))
            }
            Some("struct") => {
                let mut members = Vec::new();
                for (member_name, member) in def["members"].as_object().ok_or_else(|| field("members"))? {
                    let mut ty = self.reference(&member["type"])?;
                    // A member declared as several values in a row is an array made on the spot.
                    if !member["size"].is_null() {
                        let len = member["size"].as_u64().ok_or_else(|| field("members"))?;
                        let name = format!("Array_{}_{len}", ty.class_name());
                        let format = format_string(&member["type"]["format"]);
                        ty = Ty::Array(Arc::new(ArrayTy { name, len, format, element: ty }));
                    }
                    let index = member["index"].as_u64().ok_or_else(|| field("members"))?;
                    if members.iter().any(|(i, _)| *i == index) {
                        return Err(format!("Invalid dictionary: Duplicate index {index} in serializable type {name}"));
                    }
                    let description = member["annotation"].as_str().unwrap_or("").to_string();
                    let format = format_string(&member["format"]);
                    members.push((index, Member { name: member_name.clone(), ty, format, description }));
                }
                members.sort_by_key(|(index, _)| *index);
                let members = members.into_iter().map(|(_, m)| m).collect();
                Ok(Ty::Struct(Arc::new(StructTy { name: name.to_string(), members })))
            }
            _ => Err(format!("Type definition has unknown kind {def}")),
        }
    }
}

fn integer(value: &Value) -> Option<i128> {
    value.as_i64().map(i128::from).or_else(|| value.as_u64().map(i128::from))
}

/// An FPP format string as the Python GDS keeps it: `{x}` becomes `{:x}`; absent is `{}`.
fn format_string(format: &Value) -> String {
    let Some(text) = format.as_str() else { return "{}".into() };
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..=open]);
        rest = &rest[open + 1..];
        let Some(close) = rest.find('}') else { break };
        let spec = &rest[..close];
        let digits = spec.trim_end_matches(|c: char| "cdxoefgCDXOEFG".contains(c));
        let one_letter = spec.len() == digits.len() + 1;
        if one_letter && digits.chars().all(|c| c.is_ascii_digit() || c == '.') && digits.matches('.').count() <= 1 {
            out.push(':');
        }
    }
    out + rest
}

impl DpDictionary {
    pub fn load(path: &str) -> Result<DpDictionary, String> {
        let text = std::fs::read_to_string(path).map_err(|e| crate::io_error(&e, path))?;
        let json: Value = fgds_core::serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        DpDictionary::from_json(&json)
    }

    pub fn from_json(json: &Value) -> Result<DpDictionary, String> {
        let list = |key: &str| json[key].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let mut builder = Builder { definitions: HashMap::new(), done: HashMap::new(), active: HashSet::new() };
        for def in list("typeDefinitions") {
            if let Some(name) = def["qualifiedName"].as_str() {
                builder.definitions.entry(name).or_insert(def);
            }
        }
        let mut types = HashMap::new();
        for def in list("typeDefinitions") {
            if let Some(name) = def["qualifiedName"].as_str() {
                let reference = fgds_core::serde_json::json!({ "name": name });
                types.insert(name.to_string(), builder.reference(&reference));
            }
        }
        let mut constants = HashMap::new();
        for constant in list("constants") {
            if let (Some(name), Some(value)) = (constant["qualifiedName"].as_str(), integer(&constant["value"])) {
                constants.insert(name.to_string(), value);
            }
        }
        let text = |v: &Value| v.as_str().map(str::to_string);
        let mut records = HashMap::new();
        for record in list("records") {
            let bad = |e: &str| format!("Error parsing Data Product Record: {record}, Error: {e}");
            let id = integer(&record["id"]).ok_or_else(|| bad("id"))?;
            let def = RecordDef {
                id,
                name: text(&record["name"]).ok_or_else(|| bad("name"))?,
                ty: builder.reference(&record["type"]).map_err(|e| bad(&e))?,
                is_array: record["array"].as_bool().unwrap_or(false),
                description: text(&record["annotation"]),
            };
            records.insert(id, def);
        }
        let mut containers = HashMap::new();
        for container in list("containers") {
            let bad = |e: &str| format!("Error parsing Data Product Container: {container}, Error: {e}");
            let id = integer(&container["id"]).ok_or_else(|| bad("id"))?;
            let def = ContainerDef {
                id,
                name: text(&container["name"]).ok_or_else(|| bad("name"))?,
                default_priority: integer(&container["defaultPriority"]).ok_or_else(|| bad("defaultPriority"))?,
                description: text(&container["annotation"]),
            };
            containers.insert(id, def);
        }
        Ok(DpDictionary { types, constants, records, containers })
    }

    /// A framework or dictionary type by name, with the defaults of the Python GDS.
    pub fn get_type(&self, name: &str) -> Result<Ty, String> {
        if let Some(found) = self.types.get(name) {
            return found.clone();
        }
        let fallback_enum = |name: &str, constants: &[(&str, i128)]| {
            let constants = constants.iter().map(|(k, v)| (k.to_string(), *v)).collect();
            Ty::Enum(Arc::new(EnumTy { name: name.into(), bits: 16, signed: false, constants }))
        };
        match name {
            "FwPacketDescriptorType" | "FwTlmPacketizeIdType" | "FwSizeStoreType" => Ok(U16),
            "FwChanIdType" | "FwEventIdType" | "FwOpcodeType" => Ok(U32),
            "FwTimeContextStoreType" => Ok(U8),
            "TimeBase" => Ok(fallback_enum(
                "__GdsInternal_TimeBase_Fallback",
                &[
                    ("TB_NONE", 0),
                    ("TB_PROC_TIME", 1),
                    ("TB_WORKSTATION_TIME", 2),
                    ("TB_SC_TIME", 3),
                    ("TB_FPGA_TIME", 4),
                    ("TB_DONT_CARE", 0xffff),
                ],
            )),
            _ => Err(format!("Unknown type name: {name}")),
        }
    }

    pub fn get_constant(&self, name: &str) -> Result<i128, String> {
        match (self.constants.get(name), name) {
            (Some(value), _) => Ok(*value),
            (None, "FW_SERIALIZE_TRUE_VALUE") => Ok(0xff),
            (None, "FW_SERIALIZE_FALSE_VALUE") => Ok(0),
            _ => Err(format!("Unknown constant name: {name}")),
        }
    }

    pub fn time_type(&self) -> Result<Ty, String> {
        let (base, context) = (self.get_type("TimeBase")?, self.get_type("FwTimeContextStoreType")?);
        Ok(Ty::Time(Arc::new(TimeTy { base, context })))
    }

    /// The data product header (Fw/Dp SDD); the dictionary does not describe it.
    pub fn header_type(&self) -> Result<Arc<StructTy>, String> {
        let proc_types = match self.get_type("Fw.DpCfg.ProcType")? {
            Ty::Enum(e) => Ty::Int { bits: e.bits, signed: e.signed },
            other => return Err(format!("type object '{}' has no attribute 'REP_TYPE'", other.class_name())),
        };
        let user_data_len = self.get_constant("Fw.DpCfg.CONTAINER_USER_DATA_SIZE")?;
        let len = u64::try_from(user_data_len).map_err(|_| format!("user data size {user_data_len} is negative"))?;
        let user_data = ArrayTy { name: "UserData".into(), len, format: "{}".into(), element: U8 };
        let members = [
            ("PacketDescriptor", self.get_type("FwPacketDescriptorType")?, "The F Prime packet descriptor"),
            ("Id", self.get_type("FwDpIdType")?, "The container ID"),
            ("Priority", self.get_type("FwDpPriorityType")?, "The container priority"),
            ("Time", self.time_type()?, "Fw.Time object"),
            ("ProcTypes", proc_types, "Processing types bit mask"),
            ("UserData", Ty::Array(Arc::new(user_data)), "User-configurable data"),
            ("DpState", self.get_type("Fw.DpState")?, "Data product state"),
            ("DataSize", self.get_type("FwSizeStoreType")?, "Size of data payload in bytes"),
            ("Checksum", U32, "Header checksum"),
        ];
        let member = |(name, ty, description): (&str, Ty, &str)| Member {
            name: name.into(),
            ty,
            format: "{}".into(),
            description: description.into(),
        };
        Ok(Arc::new(StructTy {
            name: "DataProductHeaderType".into(),
            members: members.into_iter().map(member).collect(),
        }))
    }

    /// The largest encoded size in bytes (`getMaxSize`).
    pub fn max_size(&self, ty: &Ty) -> Result<u64, String> {
        Ok(match ty {
            Ty::Int { bits, .. } | Ty::Float { bits } => u64::from(*bits / 8),
            Ty::Bool => 1,
            Ty::Str { max } => self.size_store_bytes()?.saturating_add(*max),
            Ty::Enum(e) => u64::from(e.bits / 8),
            Ty::Array(a) => self.max_size(&a.element)?.saturating_mul(a.len),
            Ty::Struct(s) => {
                let mut total = 0u64;
                for member in &s.members {
                    total = total.saturating_add(self.max_size(&member.ty)?);
                }
                total
            }
            Ty::Time(t) => self.max_size(&t.base)?.saturating_add(self.max_size(&t.context)?).saturating_add(8),
        })
    }

    fn size_store_bytes(&self) -> Result<u64, String> {
        match self.get_type("FwSizeStoreType")? {
            Ty::Int { bits, .. } => Ok(u64::from(bits / 8)),
            other => Err(format!("FwSizeStoreType is {}, not an integer", other.repr())),
        }
    }

    /// Decode one value at `offset` of `data`; returns it with the bytes it used.
    /// Errors carry the text of the Python exception for the same input.
    pub fn deserialize(&self, ty: &Ty, data: &[u8], offset: usize) -> Result<(Val, usize), String> {
        match ty {
            Ty::Int { bits, .. } | Ty::Float { bits } => {
                let size = usize::from(*bits / 8);
                Ok((number(bytes_at(data, offset, size)?, ty), size))
            }
            Ty::Bool => {
                let byte = bytes_at(data, offset, 1).map_err(|_| "Not enough bytes to deserialize bool.")?[0];
                let (yes, no) =
                    (self.get_constant("FW_SERIALIZE_TRUE_VALUE")?, self.get_constant("FW_SERIALIZE_FALSE_VALUE")?);
                match i128::from(byte) {
                    value if value == yes => Ok((Val::Bool(true), 1)),
                    value if value == no => Ok((Val::Bool(false), 1)),
                    value => Err(format!("Value {value} out of range!")),
                }
            }
            Ty::Str { max } => {
                let prefix = self.size_store_bytes()? as usize;
                let short = |_| "Not enough bytes to deserialize string length.".to_string();
                let length =
                    bytes_at(data, offset, prefix).map_err(short)?.iter().fold(0u64, |acc, b| acc << 8 | u64::from(*b));
                let rest = &data[offset + prefix..];
                if (rest.len() as u64) < length {
                    return Err(format!(
                        "Not enough data to deserialize string data. Needed: {length} Left: {}",
                        rest.len()
                    ));
                }
                if length > *max {
                    return Err(format!("String size {length} is greater than {max}!"));
                }
                let bytes = &rest[..length as usize];
                let text = std::str::from_utf8(bytes).map_err(|e| utf8_error(bytes, e))?;
                Ok((Val::Str(text.to_string()), prefix + bytes.len()))
            }
            Ty::Enum(e) => {
                let size = usize::from(e.bits / 8);
                let bytes = bytes_at(data, offset, size).map_err(|_| {
                    let found = data.len().saturating_sub(offset);
                    format!("Could not deserialize enum value. Needed: {size} bytes Found: {found}")
                })?;
                let Val::Int(value) = number(bytes, &Ty::Int { bits: e.bits, signed: e.signed }) else {
                    unreachable!()
                };
                match e.constants.iter().find(|(_, v)| *v == value) {
                    Some((name, _)) => Ok((Val::Enum(name.clone(), value), size)),
                    None => Err(format!("Value {value} out of range!")),
                }
            }
            Ty::Array(a) if a.element.is_numerical() && a.len > 0 => {
                let size = self.max_size(&a.element)? as usize;
                let failed = |e: String| format!("Array NumericalType optimization failed to deserialize: {e}");
                let total = a
                    .len
                    .checked_mul(size as u64)
                    .filter(|total| *total <= (data.len() - data.len().min(offset)) as u64);
                let Some(total) = total else {
                    return Err(failed(unpack_error(data, offset, a.len.saturating_mul(size as u64))));
                };
                let bytes = &data[offset..offset + total as usize];
                Ok((
                    Val::List(bytes.chunks_exact(size).map(|chunk| number(chunk, &a.element)).collect()),
                    total as usize,
                ))
            }
            Ty::Array(a) => {
                if a.len > MAX_EMPTY_ELEMENTS && self.max_size(&a.element)? == 0 {
                    return Err(format!("Array of {} empty elements is over the limit of {MAX_EMPTY_ELEMENTS}", a.len));
                }
                let (mut items, mut used) = (Vec::new(), 0);
                for index in 0..a.len {
                    let (item, size) = self
                        .deserialize(&a.element, data, offset + used)
                        .map_err(|e| format!("Array index {index} failed to deserialize: {e}"))?;
                    items.push(item);
                    used += size;
                }
                Ok((Val::List(items), used))
            }
            Ty::Struct(s) => {
                let (mut values, mut used) = (Vec::new(), 0);
                for member in &s.members {
                    let (value, size) = self.deserialize(&member.ty, data, offset + used).map_err(|e| {
                        format!("Field '{}' [{}] failed to deserialize: {e}", member.name, member.ty.class_name())
                    })?;
                    values.push(value);
                    used += size;
                }
                Ok((Val::Struct(values), used))
            }
            Ty::Time(t) => {
                let mut used = 0;
                let mut parts = Vec::new();
                for part in [&t.base, &t.context, &U32, &U32] {
                    let (value, size) = self.deserialize(part, data, offset + used)?;
                    parts.push(Box::new(value));
                    used += size;
                }
                let parts: [Box<Val>; 4] = parts.try_into().expect("four parts");
                Ok((Val::Time(parts), used))
            }
        }
    }
}

/// The value as Python's `.val` gives it: no type information.
pub fn plain(ty: &Ty, val: &Val) -> Json {
    match (ty, val) {
        (_, Val::Int(i)) => Json::Int(*i),
        (_, Val::Float(f)) => Json::Float(*f),
        (_, Val::Bool(b)) => Json::Bool(*b),
        (_, Val::Str(s)) => Json::str(s),
        (_, Val::Enum(name, _)) => Json::str(name),
        (Ty::Array(a), Val::List(items)) => Json::List(items.iter().map(|item| plain(&a.element, item)).collect()),
        (Ty::Struct(s), Val::Struct(values)) => {
            Json::Map(s.members.iter().zip(values).map(|(m, v)| (m.name.clone(), plain(&m.ty, v))).collect())
        }
        _ => to_jsonable(ty, val),
    }
}

/// The value as Python's `to_jsonable()` gives it.
pub fn to_jsonable(ty: &Ty, val: &Val) -> Json {
    let map = |entries: Vec<(&str, Json)>| Json::Map(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    match (ty, val) {
        (Ty::Array(a), Val::List(items)) => map(vec![
            ("name", Json::str(&a.name)),
            ("type", Json::str(&a.name)),
            ("size", Json::Int(i128::from(a.len))),
            ("format", Json::str(&a.format)),
            ("value_type", Json::str(a.element.repr())),
            ("values", Json::List(items.iter().map(|item| plain(&a.element, item)).collect())),
        ]),
        (Ty::Struct(s), Val::Struct(values)) => {
            let mut members = Json::Map(Vec::new());
            for (member, value) in s.members.iter().zip(values) {
                let mut entry =
                    map(vec![("format", Json::str(&member.format)), ("description", Json::str(&member.description))]);
                if let Json::Map(fields) = to_jsonable(&member.ty, value) {
                    for (key, field) in fields {
                        entry.set(&key, field);
                    }
                }
                members.set(&member.name, entry);
            }
            members
        }
        (Ty::Time(t), Val::Time([base, context, seconds, microseconds])) => map(vec![
            ("type", Json::str("Time")),
            (
                "base",
                match base.as_ref() {
                    Val::Enum(_, number) => Json::Int(*number),
                    other => plain(&t.base, other),
                },
            ),
            ("context", plain(&t.context, context)),
            ("seconds", plain(&U32, seconds)),
            ("microseconds", plain(&U32, microseconds)),
        ]),
        _ => map(vec![("value", plain(ty, val)), ("type", Json::str(ty.repr()))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fpp_format_strings_become_python_format_strings() {
        let cases = [
            ("{}", "{}"),
            ("{x}", "{:x}"),
            ("{.2f}", "{:.2f}"),
            ("a {d} b {10.3e}", "a {:d} b {:10.3e}"),
            ("{z}", "{z}"),
            ("{", "{"),
        ];
        for (fpp, python) in cases {
            assert_eq!(format_string(&Value::String(fpp.into())), python);
        }
        assert_eq!(format_string(&Value::Null), "{}");
    }
}
