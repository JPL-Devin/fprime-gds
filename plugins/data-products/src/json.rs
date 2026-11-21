//! The JSON document `fprime-dp decode` writes, as a value that keeps what Python's
//! `json.dump(indent=2)` can write and `serde_json` cannot: exact 64-bit integers
//! beside floats, and `NaN`, `Infinity`, `-Infinity`.

use fgds_core::serde_json::{self, Value};
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Json>),
    /// Keys in insertion order, as a Python dict.
    Map(Vec<(String, Json)>),
}

impl Json {
    pub fn str(text: impl Into<String>) -> Json {
        Json::Str(text.into())
    }

    /// Set a key of a map as `dict[key] = value` does: an existing key keeps its place.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Map(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The text `json.dump(value, indent=2)` writes, byte for byte.
    pub fn to_python_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        let newline = |out: &mut String, depth: usize| {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', 2 * depth));
        };
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(i) => write!(out, "{i}").expect("write to a string"),
            Json::Float(f) => out.push_str(&python_float(*f)),
            Json::Str(s) => python_string(s, out),
            Json::List(items) if items.is_empty() => out.push_str("[]"),
            Json::Map(entries) if entries.is_empty() => out.push_str("{}"),
            Json::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "" } else { "," });
                    newline(out, depth + 1);
                    item.write(out, depth + 1);
                }
                newline(out, depth);
                out.push(']');
            }
            Json::Map(entries) => {
                out.push('{');
                for (i, (key, value)) in entries.iter().enumerate() {
                    out.push_str(if i == 0 { "" } else { "," });
                    newline(out, depth + 1);
                    python_string(key, out);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                }
                newline(out, depth);
                out.push('}');
            }
        }
    }

    /// The same document for the network API (API-4): integers a browser cannot hold
    /// exactly and non-finite floats become strings.
    pub fn to_api(&self) -> Value {
        const SAFE: i128 = (1 << 53) - 1;
        match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Int(i) if (-SAFE..=SAFE).contains(i) => Value::from(*i as i64),
            Json::Int(i) => Value::String(i.to_string()),
            Json::Float(f) => {
                serde_json::Number::from_f64(*f).map_or_else(|| Value::String(python_float(*f)), Value::Number)
            }
            Json::Str(s) => Value::String(s.clone()),
            Json::List(items) => Value::Array(items.iter().map(Json::to_api).collect()),
            Json::Map(entries) => Value::Object(entries.iter().map(|(k, v)| (k.clone(), v.to_api())).collect()),
        }
    }
}

/// `json.dumps` of a string with `ensure_ascii=True`.
fn python_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' | '\u{7f}' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    write!(out, "\\u{unit:04x}").expect("write to a string");
                }
            }
        }
    }
    out.push('"');
}

/// `float.__repr__`, with the names `json` uses for non-finite values.
pub fn python_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if f.is_infinite() {
        return format!("{sign}Infinity");
    }
    if f == 0.0 {
        return format!("{sign}0.0");
    }
    // Shortest digits that round-trip, as Python; only the layout differs.
    let scientific = format!("{:e}", f.abs());
    let (mantissa, exponent) = scientific.split_once('e').expect("exponent form");
    let exponent: i32 = exponent.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if digits.len() <= point as usize {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        }
    } else {
        let fraction = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!("{}{fraction}e{exponent_sign}{:02}", &digits[..1], exponent.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_are_written_as_python_writes_them() {
        for (value, text) in [
            (1.0, "1.0"),
            (-100.0, "-100.0"),
            (1.25, "1.25"),
            (3.1415927410125732, "3.1415927410125732"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.2345e22, "1.2345e+22"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (-0.0, "-0.0"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(python_float(value), text);
        }
    }

    #[test]
    fn text_is_python_json_dump_with_indent_2() {
        let value = Json::Map(vec![
            ("a".into(), Json::List(vec![Json::Int(1), Json::Str("é\"\n😀\u{1}".into())])),
            ("b".into(), Json::Map(vec![])),
            ("c".into(), Json::List(vec![])),
            ("d".into(), Json::Int(u64::MAX as i128)),
        ]);
        let text = "{\n  \"a\": [\n    1,\n    \"\\u00e9\\\"\\n\\ud83d\\ude00\\u0001\"\n  ],\n  \"b\": {},\n  \"c\": [],\n  \"d\": 18446744073709551615\n}";
        assert_eq!(value.to_python_text(), text);
        assert_eq!(value.to_api()["d"], "18446744073709551615");
    }
}
