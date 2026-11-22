use crate::types::{Time, Type};
use crate::Dictionary;
use serde_json::{json, Map, Value};

/// A value that cannot be encoded, or bytes that cannot be decoded, as a given type.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct TypeError(pub String);

fn fail<T>(msg: impl Into<String>) -> Result<T, TypeError> {
    Err(TypeError(msg.into()))
}

/// A cursor over packet bytes. All F Prime wire values are big-endian.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], TypeError> {
        if self.remaining() < n {
            return fail(format!("needed {n} bytes at offset {}, {} left", self.pos, self.remaining()));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// An unsigned big-endian integer of 8, 16, 32 or 64 bits.
    pub fn uint(&mut self, bits: u8) -> Result<u64, TypeError> {
        let bytes = self.take(bits as usize / 8)?;
        Ok(bytes.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64))
    }
}

fn put_uint(out: &mut Vec<u8>, bits: u8, value: u64) {
    let bytes = value.to_be_bytes();
    out.extend_from_slice(&bytes[8 - bits as usize / 8..]);
}

/// An integer given as a JSON number, or as text read as the Python GDS reads it.
fn int_from_json(value: &Value) -> Option<i128> {
    match value {
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        Value::String(s) => crate::input::python_int(s),
        // Python: `bool` is an `int`.
        Value::Bool(b) => Some(i128::from(*b)),
        Value::Object(_) => big_int(value).and_then(crate::input::python_int),
        _ => None,
    }
}

/// The member that holds the digits of an integer of JSON text beyond 64 bits (see `json_with_big_ints`).
const BIG_INT: &str = "\u{0}int";

fn big_int(value: &Value) -> Option<&str> {
    value.as_object().filter(|o| o.len() == 1).and_then(|o| o.get(BIG_INT)).and_then(Value::as_str)
}

/// JSON text as Python reads it: an integer literal of any size is an integer. `serde_json` reads one
/// beyond 64 bits as a float, so each is replaced by an object holding its digits before the text is read.
fn json_with_big_ints(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                out.push_str(text.get(start..i)?);
            }
            b'-' | b'0'..=b'9' => {
                while i < bytes.len() && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    i += 1;
                }
                let number = &text[start..i];
                let integer = number.strip_prefix('-').unwrap_or(number).bytes().all(|b| b.is_ascii_digit());
                if integer && number.parse::<i64>().is_err() && number.parse::<u64>().is_err() {
                    out.push_str(&json!({ BIG_INT: number }).to_string());
                } else {
                    out.push_str(number);
                }
            }
            _ => {
                i += 1;
                while !text.is_char_boundary(i) {
                    i += 1;
                }
                out.push_str(&text[start..i]);
            }
        }
    }
    serde_json::from_str(&out).ok()
}

/// The type rule of the Python GDS for the members of an array or a struct given as JSON text
/// (`validate` of its types): a number where a number is expected, never its text; `true` and `false`
/// are integers too, an integer is a float. The message is the one of `TypeMismatchException`.
fn python_json_types(ty: &Type, value: &Value) -> Result<(), TypeError> {
    let class = |value: &Value| match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) if big_int(value).is_some() => "int",
        Value::Object(_) => "dict",
    };
    let expect = |name: &str, accepted: &[&str]| match accepted.contains(&class(value)) {
        true => Ok(()),
        false => fail(format!("Type <class '{name}'> expected, type <class '{}'> found!", class(value))),
    };
    match ty {
        Type::Int { .. } => expect("int", &["int", "bool"]),
        Type::Float { .. } => expect("float", &["float", "int", "bool"]),
        Type::Bool => expect("bool", &["bool"]),
        Type::String { .. } | Type::Enum { .. } => expect("str", &["str"]),
        Type::Array { element, .. } => {
            value.as_array().into_iter().flatten().try_for_each(|v| python_json_types(element, v))
        }
        Type::Struct { members, .. } => members.iter().try_for_each(|m| match (value.get(&m.name), m.count) {
            (Some(v), None) => python_json_types(&m.ty, v),
            (Some(v), Some(_)) => v.as_array().into_iter().flatten().try_for_each(|v| python_json_types(&m.ty, v)),
            (None, _) => Ok(()),
        }),
    }
}

fn float_to_json(v: f64) -> Value {
    if v.is_nan() {
        json!("NaN")
    } else if v.is_infinite() {
        json!(if v > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        json!(v)
    }
}

impl Dictionary {
    /// A time tag: time base, time context, seconds, microseconds.
    pub fn decode_time(&self, r: &mut Reader) -> Result<Time, TypeError> {
        let bits = |name: &str, default: u8| match self.types.get(name) {
            Some(Type::Int { bits, .. }) => *bits,
            _ => default,
        };
        Ok(Time {
            base: r.uint(bits("FwTimeBaseStoreType", 16))? as u16,
            context: r.uint(bits("FwTimeContextStoreType", 8))? as u8,
            seconds: r.uint(32)? as u32,
            microseconds: r.uint(32)? as u32,
        })
    }

    /// The largest encoded size of a type: what a telemetry packet reserves for a channel.
    pub fn max_size(&self, ty: &Type) -> usize {
        match ty {
            Type::Int { bits, .. } | Type::Float { bits } => *bits as usize / 8,
            Type::Bool => 1,
            Type::String { max } => self.max_size(&self.size_store).saturating_add(*max),
            Type::Enum { rep, .. } => self.max_size(rep),
            Type::Array { size, element, .. } => size.saturating_mul(self.max_size(element)),
            Type::Struct { members, .. } => members
                .iter()
                .fold(0usize, |sum, m| sum.saturating_add(m.count.unwrap_or(1).saturating_mul(self.max_size(&m.ty)))),
        }
    }

    /// Decode one value of type `ty` from `r`.
    pub fn decode(&self, ty: &Type, r: &mut Reader) -> Result<Value, TypeError> {
        match ty {
            Type::Int { bits, signed } => {
                let raw = r.uint(*bits)?;
                Ok(match (*bits, *signed) {
                    (64, false) => json!(raw.to_string()),
                    (64, true) => json!((raw as i64).to_string()),
                    (_, false) => json!(raw),
                    // Sign-extend from the encoded width.
                    (bits, true) => json!(((raw << (64 - bits)) as i64) >> (64 - bits)),
                })
            }
            Type::Float { bits: 32 } => Ok(float_to_json(f32::from_bits(r.uint(32)? as u32) as f64)),
            Type::Float { .. } => Ok(float_to_json(f64::from_bits(r.uint(64)?))),
            Type::Bool => match r.uint(8)? as u8 {
                b if b == self.bool_true => Ok(json!(true)),
                b if b == self.bool_false => Ok(json!(false)),
                b => fail(format!("invalid boolean byte 0x{b:02X}")),
            },
            Type::String { max } => {
                let Type::Int { bits, .. } = self.size_store else { unreachable!("checked at load") };
                let len = r.uint(bits)? as usize;
                if len > *max {
                    return fail(format!("string length {len} exceeds maximum {max}"));
                }
                match std::str::from_utf8(r.take(len)?) {
                    Ok(s) => Ok(json!(s)),
                    Err(_) => fail("string is not valid UTF-8"),
                }
            }
            Type::Enum { name, rep, constants } => {
                let raw = int_from_json(&self.decode(rep, r)?).unwrap_or_default();
                match constants.iter().find(|(_, v)| *v as i128 == raw) {
                    Some((constant, _)) => Ok(json!(constant)),
                    None => fail(format!("{raw} is not a constant of enum {name}")),
                }
            }
            Type::Array { size, element, .. } => {
                (0..*size).map(|_| self.decode(element, r)).collect::<Result<Vec<_>, _>>().map(Value::Array)
            }
            Type::Struct { members, .. } => {
                let mut out = Map::new();
                for m in members {
                    let value = match m.count {
                        None => self.decode(&m.ty, r)?,
                        Some(n) => Value::Array((0..n).map(|_| self.decode(&m.ty, r)).collect::<Result<_, _>>()?),
                    };
                    out.insert(m.name.clone(), value);
                }
                Ok(Value::Object(out))
            }
        }
    }

    /// Encode `value` as type `ty`, appending to `out`. The value is validated against the type.
    pub fn encode(&self, ty: &Type, value: &Value, out: &mut Vec<u8>) -> Result<(), TypeError> {
        // An array or a struct given as text is JSON, as on the command line of the Python GDS.
        let parsed = match (ty, value) {
            (Type::Array { .. } | Type::Struct { .. }, Value::String(text)) => json_with_big_ints(text),
            _ => None,
        };
        if let Some(parsed) = &parsed {
            python_json_types(ty, parsed)?;
        }
        let value = parsed.as_ref().unwrap_or(value);
        match ty {
            Type::Int { bits, signed } => {
                let name = format!("{}{bits}", if *signed { 'I' } else { 'U' });
                let Some(v) = int_from_json(value) else {
                    return fail(format!("{value} is not an integer ({name})"));
                };
                let (min, max) = match signed {
                    true => (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1),
                    false => (0, (1i128 << bits) - 1),
                };
                if v < min || v > max {
                    return fail(format!("{v} is out of range for {name}"));
                }
                put_uint(out, *bits, v as u64);
            }
            Type::Float { bits } => {
                let v = match value {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => crate::input::python_float(s),
                    Value::Bool(b) => Some(f64::from(u8::from(*b))),
                    Value::Object(_) => big_int(value).and_then(crate::input::python_float),
                    _ => None,
                };
                match (v, bits) {
                    (None, _) => return fail(format!("{value} is not a number (F{bits})")),
                    // A finite value beyond F32 would be sent as infinity.
                    (Some(v), 32) if v.is_finite() && !(v as f32).is_finite() => {
                        return fail(format!("{v} is out of range for F32"));
                    }
                    (Some(v), 32) => put_uint(out, 32, (v as f32).to_bits() as u64),
                    (Some(v), _) => put_uint(out, 64, v.to_bits()),
                }
            }
            Type::Bool => {
                let v = match value {
                    Value::Bool(b) => Some(*b),
                    Value::String(s) => crate::input::python_bool(s),
                    _ => None,
                };
                match v {
                    Some(true) => out.push(self.bool_true),
                    Some(false) => out.push(self.bool_false),
                    None => return fail(format!("{value} is not a boolean")),
                }
            }
            Type::String { max } => {
                let Some(s) = value.as_str() else {
                    return fail(format!("{value} is not a string"));
                };
                if s.len() > *max {
                    return fail(format!("string of {} bytes exceeds maximum {max}", s.len()));
                }
                let Type::Int { bits, .. } = self.size_store else { unreachable!("checked at load") };
                put_uint(out, bits, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            Type::Enum { name, rep, constants } => {
                let found = value.as_str().and_then(|s| constants.iter().find(|(c, _)| c == s));
                match found {
                    Some((_, v)) => self.encode(rep, &json!(v), out)?,
                    None => return fail(format!("{value} is not a constant of enum {name}")),
                }
            }
            Type::Array { name, size, element, .. } => self.encode_list(name, element, *size, value, out)?,
            Type::Struct { name, members } => {
                let Some(map) = value.as_object() else {
                    return fail(format!("{value} is not an object (struct {name})"));
                };
                if let Some(extra) = map.keys().find(|k| !members.iter().any(|m| &m.name == *k)) {
                    return fail(format!("struct {name} has no member {extra}"));
                }
                for m in members {
                    let Some(v) = map.get(&m.name) else {
                        return fail(format!("struct {name}: member {} is missing", m.name));
                    };
                    let at = |e: TypeError| TypeError(format!("{name}.{}: {}", m.name, e.0));
                    match m.count {
                        None => self.encode(&m.ty, v, out).map_err(at)?,
                        Some(n) => self.encode_list(&m.name, &m.ty, n, v, out).map_err(at)?,
                    }
                }
            }
        }
        Ok(())
    }

    fn encode_list(
        &self,
        name: &str,
        element: &Type,
        size: usize,
        value: &Value,
        out: &mut Vec<u8>,
    ) -> Result<(), TypeError> {
        match value.as_array() {
            Some(items) if items.len() == size => items.iter().enumerate().try_for_each(|(i, v)| {
                self.encode(element, v, out).map_err(|e| TypeError(format!("{name}[{i}]: {}", e.0)))
            }),
            _ => fail(format!("{value} is not an array of {size} elements ({name})")),
        }
    }
}

/// A command turned into bytes by `Dictionary::encode_command`.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedCommand {
    pub opcode: u32,
    /// The opcode then the arguments, as they go in a command packet after the packet type.
    pub bytes: Vec<u8>,
    /// The arguments by name in declaration order, as sent: decoded back from `bytes`.
    pub args: Map<String, Value>,
}

impl Dictionary {
    /// Encode the command `name`: the opcode, then the arguments in declaration order.
    /// `args` is a list in declaration order, an object by argument name, or null for none.
    /// Used by the command encoder and by the sequence compiler.
    pub fn encode_command(&self, name: &str, args: &Value) -> Result<EncodedCommand, TypeError> {
        let command = self.command(name).ok_or_else(|| TypeError(format!("unknown command {name}")))?;
        let params = &command.params;
        let count = |n: usize| match n == params.len() {
            true => Ok(()),
            false => Err(TypeError(format!("{name} takes {} arguments, {n} given", params.len()))),
        };
        // Arguments in declaration order, whichever way they were given.
        let values: Vec<&Value> = match args {
            Value::Null => count(0).map(|_| Vec::new())?,
            Value::Array(values) => count(values.len()).map(|_| values.iter().collect())?,
            Value::Object(by_name) => {
                if let Some(extra) = by_name.keys().find(|k| !params.iter().any(|p| p.name == **k)) {
                    return Err(TypeError(format!("{name} has no argument {extra}")));
                }
                let find = |p: &crate::Param| {
                    by_name.get(&p.name).ok_or_else(|| TypeError(format!("argument {} is missing", p.name)))
                };
                params.iter().map(find).collect::<Result<_, _>>()?
            }
            _ => return Err(TypeError("\"args\" is neither a list nor an object".into())),
        };
        let mut bytes = Vec::new();
        let opcode_type = self.config_type("FwOpcodeType", Type::U32);
        self.encode(&opcode_type, &json!(command.opcode), &mut bytes)
            .map_err(|e| TypeError(format!("opcode: {}", e.0)))?;
        let mut sent = Map::new();
        for (param, value) in params.iter().zip(values) {
            let start = bytes.len();
            self.encode(&param.ty, value, &mut bytes)
                .map_err(|e| TypeError(format!("argument {}: {}", param.name, e.0)))?;
            // What was sent, not what was asked: decode the bytes just written.
            let decoded = self.decode(&param.ty, &mut Reader::new(&bytes[start..]));
            sent.insert(param.name.clone(), decoded.unwrap_or_else(|_| value.clone()));
        }
        Ok(EncodedCommand { opcode: command.opcode, bytes, args: sent })
    }
}

#[cfg(test)]
mod big_int_tests {
    use super::*;

    #[test]
    fn integers_beyond_64_bits_keep_their_digits() {
        let value = json_with_big_ints(r#"{"a": 18446744073709551616, "b": [-9223372036854775809, 1e30, 7], "c": "9 99999999999999999999 \" é"}"#).unwrap();
        assert_eq!(big_int(&value["a"]), Some("18446744073709551616"));
        assert_eq!(big_int(&value["b"][0]), Some("-9223372036854775809"));
        assert!(value["b"][1].is_f64() && value["b"][2] == 7);
        assert_eq!(value["c"], "9 99999999999999999999 \" é");
        assert_eq!(int_from_json(&value["a"]), Some(18446744073709551616));
        assert!(json_with_big_ints("{").is_none());
    }
}
