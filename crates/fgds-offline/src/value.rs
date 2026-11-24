//! The values of sequence arguments and parameter files, with the text forms Python gives them.

use fgds_core::dictionary::{float_json, float_text};
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
    /// The name Python prints for the type of this value.
    pub fn python_type(&self) -> &'static str {
        match self {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Array(_) => "list",
            Value::Object(_) => "dict",
        }
    }

    pub fn object(pairs: impl IntoIterator<Item = (String, Value)>) -> Value {
        let mut out: Vec<(String, Value)> = Vec::new();
        for (key, value) in pairs {
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => out.push((key, value)),
            }
        }
        Value::Object(out)
    }

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

    /// Parse JSON text.
    pub fn parse_json(text: &str) -> Result<Value, String> {
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => Ok(Value::from_json(&v)),
            Err(e) => Err(format!("invalid JSON: {e}")),
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

    pub fn to_json(&self) -> String {
        self.to_serde().to_string()
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.to_serde()).unwrap_or_default()
    }

    /// The text of Python's `str()`, for scalars; containers print as JSON.
    pub fn python_str(&self) -> String {
        match self {
            Value::Null => "None".into(),
            Value::Bool(b) => if *b { "True" } else { "False" }.into(),
            Value::Float(f) => float_text(*f),
            Value::Str(s) => s.clone(),
            other => other.to_json(),
        }
    }
}

/// Python's `repr()` of a string.
pub fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            ' '..='~' => out.push(c),
            c if (c as u32) < 0x100 && (c.is_control() || c == '\u{a0}' || c == '\u{ad}') => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if c.is_control() || (c.is_whitespace() && c != ' ') => {
                let _ = if (c as u32) < 0x10000 {
                    write!(out, "\\u{:04x}", c as u32)
                } else {
                    write!(out, "\\U{:08x}", c as u32)
                };
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `float()` of a string: surrounding blanks, underscores between digits, `inf`, `nan`.
pub fn parse_float(text: &str) -> Option<f64> {
    let t = text.trim();
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    match body.to_ascii_lowercase().as_str() {
        "inf" | "infinity" => return Some(if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }),
        "nan" => return Some(f64::NAN),
        _ => {}
    }
    let chars: Vec<char> = body.chars().collect();
    let valid = !chars.is_empty()
        && chars.iter().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-' | '_'))
        && chars.iter().enumerate().all(|(i, c)| {
            *c != '_' || (i > 0 && chars[i - 1].is_ascii_digit() && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        })
        && chars.iter().any(char::is_ascii_digit);
    if !valid {
        return None;
    }
    // A literal beyond a double is refused: only `inf` spelled out is infinity.
    t.replace('_', "").parse().ok().filter(|v: &f64| v.is_finite())
}

/// Python's `int(text, 0)`: a sign, then decimal digits without a leading zero, or digits
/// after `0x`, `0o` or `0b`; underscores between digits.
pub fn parse_int(text: &str) -> Option<Int> {
    let t = text.trim();
    let (sign, body) = match t.strip_prefix(['+', '-']) {
        Some(rest) => (&t[..1], rest),
        None => ("", t),
    };
    let lower = body.to_ascii_lowercase();
    let (radix, digits) = match lower.get(..2) {
        Some("0x") => (16, &body[2..]),
        Some("0o") => (8, &body[2..]),
        Some("0b") => (2, &body[2..]),
        _ => (10, body),
    };
    let chars: Vec<char> = digits.chars().collect();
    let digit = |c: &char| c.is_digit(radix);
    // An underscore stands between two digits, or between the prefix and the first digit.
    let valid = chars.last().is_some_and(digit)
        && chars
            .iter()
            .enumerate()
            .all(|(i, c)| digit(c) || (*c == '_' && chars.get(i + 1).is_some_and(digit) && (i > 0 || radix != 10)))
        && (radix != 10 || chars[0] != '0' || chars.iter().all(|c| matches!(c, '0' | '_')));
    if !valid {
        return None;
    }
    Int::parse(&format!("{sign}{digits}"), radix)
}
