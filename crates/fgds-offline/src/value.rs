//! The values of sequence arguments and parameter files, with the text forms Python gives them.

use fgds_core::dictionary::float_json;
use fgds_core::serde_json;
use std::fmt::{self, Write};

/// An integer of any size, as Python has them: sign and decimal digits without leading zeros.
#[derive(Debug, Clone, PartialEq)]
pub struct Int {
    negative: bool,
    digits: String,
}

impl Int {
    /// From digits in `radix` (10 or 16) with an optional sign; underscores are skipped.
    pub fn parse(text: &str, radix: u32) -> Option<Int> {
        let (negative, body) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        // Little-endian limbs in base 10^9.
        let mut limbs: Vec<u64> = vec![0];
        let mut any = false;
        for c in body.chars().filter(|c| *c != '_') {
            let mut carry = u64::from(c.to_digit(radix)?);
            any = true;
            for limb in &mut limbs {
                let v = *limb * u64::from(radix) + carry;
                *limb = v % 1_000_000_000;
                carry = v / 1_000_000_000;
            }
            if carry > 0 {
                limbs.push(carry);
            }
        }
        if !any {
            return None;
        }
        let mut digits = limbs.pop().unwrap_or(0).to_string();
        for limb in limbs.iter().rev() {
            let _ = write!(digits, "{limb:09}");
        }
        Some(Int { negative: negative && digits != "0", digits })
    }

    pub fn to_i128(&self) -> Option<i128> {
        let v: i128 = self.digits.parse().ok()?;
        Some(if self.negative { -v } else { v })
    }

    /// The nearest float, `None` when it is too large for one (Python raises `OverflowError`).
    pub fn to_f64(&self) -> Option<f64> {
        let v: f64 = self.digits.parse().ok()?;
        v.is_finite().then_some(if self.negative { -v } else { v })
    }
}

impl From<i128> for Int {
    fn from(v: i128) -> Self {
        Int { negative: v < 0, digits: v.unsigned_abs().to_string() }
    }
}

impl fmt::Display for Int {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", if self.negative { "-" } else { "" }, self.digits)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(Int),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    /// Keys in the order written; a repeated key keeps its first place and its last value.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn from_json(v: &serde_json::Value) -> Value {
        use serde_json::Value as J;
        match v {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(*b),
            J::Number(n) => match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => Value::Int(i128::from(i).into()),
                (_, Some(u)) => Value::Int(i128::from(u).into()),
                _ => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            J::String(s) => Value::Str(s.clone()),
            J::Array(items) => Value::Array(items.iter().map(Value::from_json).collect()),
            J::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), Value::from_json(v))).collect()),
        }
    }

    /// As JSON: an integer beyond 64 bits and a float that is no number are text.
    pub fn to_serde(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => match i.to_i128() {
                Some(v) if i64::try_from(v).is_ok() => J::from(v as i64),
                Some(v) if u64::try_from(v).is_ok() => J::from(v as u64),
                _ => J::String(i.to_string()),
            },
            Value::Float(f) => float_json(*f),
            Value::Str(s) => J::String(s.clone()),
            Value::Array(items) => J::Array(items.iter().map(Value::to_serde).collect()),
            Value::Object(pairs) => J::Object(pairs.iter().map(|(k, v)| (k.clone(), v.to_serde())).collect()),
        }
    }

    /// JSON on one line, as a sequence file carries it in a string argument: `, ` and `: ` between
    /// the parts and ASCII only, so that the bytes sent to flight stay those of `fprime-seqgen`.
    pub fn to_json(&self) -> String {
        use serde::Serialize;
        let mut out = Vec::new();
        let mut writer = serde_json::Serializer::with_formatter(&mut out, Spaced);
        match self.to_serde().serialize(&mut writer) {
            Ok(()) => String::from_utf8(out).unwrap_or_default(),
            Err(_) => String::new(),
        }
    }
}

struct Spaced;

impl serde_json::ser::Formatter for Spaced {
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        w.write_all(if first { b"" } else { b", " })
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        w.write_all(if first { b"" } else { b", " })
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }

    fn write_string_fragment<W: ?Sized + std::io::Write>(&mut self, w: &mut W, text: &str) -> std::io::Result<()> {
        for c in text.chars() {
            match c.is_ascii() {
                true => w.write_all(&[c as u8])?,
                false => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        write!(w, "\\u{unit:04x}")?;
                    }
                }
            }
        }
        Ok(())
    }
}
