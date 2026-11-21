//! Display text: F Prime format strings filled with decoded values, written as the
//! Python GDS writes them so that both show the same text.

use crate::types::Type;
use crate::Dictionary;
use serde_json::Value;

/// A decoded value as Python's `str()` shows it; `nested` values are shown as `repr()`.
fn plain(ty: &Type, value: &Value, nested: bool) -> String {
    match (ty, value) {
        (Type::Bool, Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        (Type::Float { .. }, Value::Number(n)) => float(n.as_f64().unwrap_or(0.0)),
        (Type::Float { .. }, Value::String(s)) => match s.as_str() {
            "Infinity" => "inf".into(),
            "-Infinity" => "-inf".into(),
            _ => "nan".into(),
        },
        (Type::Int { .. }, Value::String(s)) => s.clone(),
        (Type::Array { element, .. }, Value::Array(items)) => {
            let items: Vec<_> = items.iter().map(|v| plain(element, v, true)).collect();
            format!("[{}]", items.join(", "))
        }
        (Type::Struct { members, .. }, Value::Object(fields)) => {
            let fields: Vec<_> = members
                .iter()
                .filter_map(|m| Some((m, fields.get(&m.name)?)))
                .map(|(m, v)| match (m.count, v) {
                    (Some(_), Value::Array(items)) => {
                        let items: Vec<_> = items.iter().map(|v| plain(&m.ty, v, true)).collect();
                        format!("'{}': [{}]", m.name, items.join(", "))
                    }
                    _ => format!("'{}': {}", m.name, plain(&m.ty, v, true)),
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        (_, Value::String(s)) if nested => format!("'{s}'"),
        (_, Value::String(s)) => s.clone(),
        (_, other) => other.to_string(),
    }
}

/// A channel value as the Python GDS shows it: in arrays and structs every leaf is text.
fn leaves_as_text(ty: &Type, value: &Value) -> String {
    let list = |ty: &Type, items: &[Value]| {
        let items: Vec<_> = items.iter().map(|v| leaves_as_text(ty, v)).collect();
        format!("[{}]", items.join(", "))
    };
    match (ty, value) {
        (Type::Array { element, .. }, Value::Array(items)) => list(element, items),
        (Type::Struct { members, .. }, Value::Object(fields)) => {
            let fields: Vec<_> = members
                .iter()
                .filter_map(|m| Some((m, fields.get(&m.name)?)))
                .map(|(m, v)| match (m.count, v) {
                    (Some(_), Value::Array(items)) => format!("'{}': {}", m.name, list(&m.ty, items)),
                    _ => format!("'{}': {}", m.name, leaves_as_text(&m.ty, v)),
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        _ => format!("'{}'", plain(ty, value, false)),
    }
}

/// A float as Python's `repr()` shows it.
fn float(v: f64) -> String {
    let magnitude = v.abs();
    if v != 0.0 && !(1e-4..1e16).contains(&magnitude) {
        return exponent(format!("{v:e}"));
    }
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        v.to_string()
    }
}

/// Rust writes `1.5e7` and `1.5e-7`; Python and C write `1.5e+07` and `1.5e-07`.
fn exponent(text: String) -> String {
    let Some((mantissa, exp)) = text.split_once(['e', 'E']) else {
        return text;
    };
    let letter = if text.contains('E') { 'E' } else { 'e' };
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(digits) => ('-', digits),
        None => ('+', exp),
    };
    format!("{mantissa}{letter}{sign}{digits:0>2}")
}

/// One value under a conversion such as `x` or `.2f`; `None` when they do not fit.
fn convert(spec: &str, ty: &Type, value: &Value) -> Option<String> {
    let conversion = spec.chars().last()?;
    let (width, precision) = match spec[..spec.len() - 1].split_once('.') {
        Some((w, p)) => (w, p.parse::<usize>().ok()),
        None => (&spec[..spec.len() - 1], None),
    };
    let width: usize = if width.is_empty() { 0 } else { width.parse().ok()? };
    let int = || match value {
        Value::Number(n) => n.as_i64().map(i128::from),
        Value::String(s) => s.parse::<i128>().ok(),
        _ => None,
    };
    let number = || match (ty, value) {
        (Type::Float { .. }, Value::String(_)) => None,
        _ => value.as_f64().or_else(|| int().map(|i| i as f64)),
    };
    let precision_or_6 = precision.unwrap_or(6);
    let text = match conversion {
        'd' | 'D' => int()?.to_string(),
        'x' => format!("{:x}", int()?),
        'X' => format!("{:X}", int()?),
        'o' | 'O' => format!("{:o}", int()?),
        'c' | 'C' => char::from_u32(int()? as u32)?.to_string(),
        'f' | 'F' => format!("{:.*}", precision_or_6, number()?),
        'e' => exponent(format!("{:.*e}", precision_or_6, number()?)),
        'E' => exponent(format!("{:.*E}", precision_or_6, number()?)),
        // `g` picks between `f` and `e`; the shortest exact form is close and simpler.
        _ => float(number()?),
    };
    // Numbers are right-aligned in their width; a character is left-aligned.
    Some(match conversion {
        'c' | 'C' => format!("{text:<width$}"),
        _ => format!("{text:>width$}"),
    })
}

impl Dictionary {
    /// The value as text, without a format string.
    pub fn display(&self, ty: &Type, value: &Value) -> String {
        plain(ty, value, false)
    }

    /// The value of a channel as text, without a format string.
    pub fn display_channel(&self, ty: &Type, value: &Value) -> String {
        match ty {
            Type::Array { .. } | Type::Struct { .. } => leaves_as_text(ty, value),
            _ => plain(ty, value, false),
        }
    }

    /// Fill an FPP format string: `{}` takes the next value as it is, `{x}`, `{.2f}`
    /// and the like convert it, `{{` and `}}` are braces. A replacement field that does
    /// not fit its value is left as written.
    pub fn format(&self, template: &str, values: &[(&Type, &Value)]) -> String {
        let mut out = String::new();
        let mut values = values.iter();
        let mut rest = template;
        while let Some(at) = rest.find(['{', '}']) {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];
            if tail.starts_with("{{") || tail.starts_with("}}") {
                out.push_str(&tail[..1]);
                rest = &tail[2..];
                continue;
            }
            let end = tail.find('}').unwrap_or(0);
            let filled = match (tail.starts_with('{') && end > 0, values.next()) {
                (true, Some((ty, value))) if end == 1 => Some(plain(ty, value, false)),
                (true, Some((ty, value))) => convert(&tail[1..end], ty, value),
                _ => None,
            };
            match filled {
                Some(text) => {
                    out.push_str(&text);
                    rest = &tail[end + 1..];
                }
                None => {
                    out.push_str(&tail[..1]);
                    rest = &tail[1..];
                }
            }
        }
        out.push_str(rest);
        out
    }
}
