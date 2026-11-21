//! The values of sequence arguments and parameter files, with the text forms Python gives them.

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

    /// Parse JSON text, with the message of Python's `json.loads` for text that is not JSON at all.
    pub fn parse_json(text: &str) -> Result<Value, String> {
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => Ok(Value::from_json(&v)),
            Err(_) if !text.trim_start().starts_with(['[', '{', '"']) => {
                Err("Expecting value: line 1 column 1 (char 0)".into())
            }
            Err(e) => Err(format!("invalid JSON: {e}")),
        }
    }

    /// The text of Python's `json.dumps`.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, None, 0);
        out
    }

    /// The text of Python's `json.dumps(indent=4)`.
    pub fn to_json_pretty(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, Some(4), 0);
        out
    }

    fn write_json(&self, out: &mut String, indent: Option<usize>, depth: usize) {
        let open = |out: &mut String, depth: usize| {
            if let Some(n) = indent {
                out.push('\n');
                out.push_str(&" ".repeat(n * depth));
            }
        };
        let separator = if indent.is_some() { "," } else { ", " };
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Float(f) if f.is_nan() => out.push_str("NaN"),
            Value::Float(f) if f.is_infinite() => out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" }),
            Value::Float(f) => out.push_str(&float_repr(*f)),
            Value::Str(s) => json_string(out, s),
            Value::Array(items) if items.is_empty() => out.push_str("[]"),
            Value::Object(pairs) if pairs.is_empty() => out.push_str("{}"),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(separator);
                    }
                    open(out, depth + 1);
                    item.write_json(out, indent, depth + 1);
                }
                open(out, depth);
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (i, (key, value)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(separator);
                    }
                    open(out, depth + 1);
                    json_string(out, key);
                    out.push_str(": ");
                    value.write_json(out, indent, depth + 1);
                }
                open(out, depth);
                out.push('}');
            }
        }
    }

    /// The text of Python's `str()`, for scalars; containers print as JSON.
    pub fn python_str(&self) -> String {
        match self {
            Value::Null => "None".into(),
            Value::Bool(b) => if *b { "True" } else { "False" }.into(),
            Value::Float(f) => float_repr(*f),
            Value::Str(s) => s.clone(),
            other => other.to_json(),
        }
    }
}

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Python's `repr()` of a float.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    // Shortest digits that read back as `f`, as `d.ddde<exp>`.
    let scientific = format!("{f:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if !(-4..16).contains(&exponent) {
        let fraction = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{}{fraction}e{exponent_sign}{:02}", &digits[..1], exponent.abs());
    }
    if exponent < 0 {
        return format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize));
    }
    let point = exponent as usize + 1;
    if digits.len() <= point {
        format!("{sign}{digits}{}.0", "0".repeat(point - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..point], &digits[point..])
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
    t.replace('_', "").parse().ok()
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
