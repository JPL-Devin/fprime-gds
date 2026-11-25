use serde_json::Value;

/// A fully resolved F Prime type: named types are expanded in place.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int {
        bits: u8,
        signed: bool,
    },
    Float {
        bits: u8,
    },
    Bool,
    /// `max` is the largest encoded length in bytes.
    String {
        max: usize,
    },
    Enum {
        name: String,
        rep: Box<Type>,
        constants: Vec<(String, i64)>,
    },
    Array {
        name: String,
        size: usize,
        element: Box<Type>,
        /// The format string of each element, when the dictionary gives one.
        format: Option<String>,
    },
    /// Members in wire order. A member with `count > 1` is that many values in a row.
    Struct {
        name: String,
        members: Vec<Member>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    pub ty: Type,
    pub count: Option<usize>,
    /// The format string of the member, when the dictionary gives one.
    pub format: Option<String>,
    /// The annotation of the member in the dictionary.
    pub description: Option<String>,
}

impl Type {
    /// The name of the type as FPP writes it: `U32`, `F64`, `bool`, `string`, or the qualified name.
    pub fn name(&self) -> String {
        match self {
            Type::Int { bits, signed } => format!("{}{bits}", if *signed { 'I' } else { 'U' }),
            Type::Float { bits } => format!("F{bits}"),
            Type::Bool => "bool".into(),
            Type::String { .. } => "string".into(),
            Type::Enum { name, .. } | Type::Array { name, .. } | Type::Struct { name, .. } => name.clone(),
        }
    }

    pub const U8: Type = Type::Int { bits: 8, signed: false };
    pub const U16: Type = Type::Int { bits: 16, signed: false };
    pub const U32: Type = Type::Int { bits: 32, signed: false };
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub opcode: u32,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub name: String,
    pub id: u32,
    pub severity: String,
    pub format: String,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub name: String,
    pub id: u32,
    pub ty: Type,
    pub format: Option<String>,
}

/// A flight time tag as it is on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub base: u16,
    pub context: u8,
    pub seconds: u32,
    pub microseconds: u32,
}

pub(crate) fn str_field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("missing string field \"{key}\""))
}

pub(crate) fn u64_field(v: &Value, key: &str) -> Result<u64, String> {
    v[key].as_u64().ok_or_else(|| format!("missing unsigned integer field \"{key}\""))
}
