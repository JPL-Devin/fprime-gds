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

/// A channel value as the Python GDS shows it: in arrays and structs every leaf is
/// text, filled into the format string of its array element or struct member.
fn leaves_as_text(dict: &Dictionary, ty: &Type, value: &Value, format: Option<&str>) -> Value {
    let list = |ty: &Type, items: &[Value], format: Option<&str>| {
        Value::Array(items.iter().map(|v| leaves_as_text(dict, ty, v, format)).collect())
    };
    match (ty, value) {
        (Type::Array { element, format, .. }, Value::Array(items)) => list(element, items, format.as_deref()),
        (Type::Struct { members, .. }, Value::Object(fields)) => Value::Object(
            members
                .iter()
                .filter_map(|m| Some((m, fields.get(&m.name)?)))
                .map(|(m, v)| match (m.count, v) {
                    (Some(_), Value::Array(items)) => (m.name.clone(), list(&m.ty, items, m.format.as_deref())),
                    _ => (m.name.clone(), leaves_as_text(dict, &m.ty, v, m.format.as_deref())),
                })
                .collect(),
        ),
        _ => Value::String(match format {
            Some(format) => dict.format(format, &[(ty, value)]),
            None => plain(ty, value, false),
        }),
    }
}

/// Lists, dictionaries and text as Python's `str()` shows a list or a dictionary.
fn python_text(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{}]", items.iter().map(python_text).collect::<Vec<_>>().join(", ")),
        Value::Object(fields) => {
            let fields: Vec<_> = fields.iter().map(|(name, v)| format!("'{name}': {}", python_text(v))).collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::String(text) => format!("'{text}'"),
        other => other.to_string(),
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

/// `%g` as C and Python write it: `precision` significant digits, the exponent form
/// when the exponent is below -4 or not below the precision, trailing zeros removed.
fn general(v: f64, precision: usize, upper: bool) -> String {
    let precision = precision.max(1);
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.into();
    }
    // The exponent after rounding to the precision.
    let scientific = format!("{:.*e}", precision - 1, v);
    let exp: i32 = scientific.split_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0);
    let trim = |text: &str| match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').to_string(),
        false => text.to_string(),
    };
    if exp < -4 || exp >= precision as i32 {
        let (mantissa, _) = scientific.split_once('e').unwrap_or((&scientific, ""));
        let text = exponent(format!("{}e{exp}", trim(mantissa)));
        return if upper { text.to_uppercase() } else { text };
    }
    trim(&format!("{:.*}", (precision as i32 - 1 - exp).max(0) as usize, v))
}

/// One value under a conversion such as `x`, `04X` or `.2f`, as Python's `format`
/// fills it (the Python GDS turns `{04X}` into `{:04X}`); `None` when they do not fit.
fn convert(spec: &str, ty: &Type, value: &Value) -> Option<String> {
    let conversion = spec.chars().last()?;
    let mut rest = &spec[..spec.len() - conversion.len_utf8()];
    let plus = rest.starts_with('+');
    rest = rest.strip_prefix('+').unwrap_or(rest);
    let (width, precision) = match rest.split_once('.') {
        Some((w, p)) => (w, Some(p.parse::<usize>().ok()?)),
        None => (rest, None),
    };
    let zero = width.starts_with('0');
    let width: usize = if width.is_empty() { 0 } else { width.parse().ok()? };
    let int = || match value {
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        Value::String(s) if matches!(ty, Type::Int { .. }) => s.parse::<i128>().ok(),
        _ => None,
    };
    let number = || match (ty, value) {
        (Type::Float { .. }, Value::String(s)) => Some(match s.as_str() {
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            _ => f64::NAN,
        }),
        (_, Value::String(_)) => None,
        _ => value.as_f64().or_else(|| int().map(|i| i as f64)),
    };
    // Sign and magnitude apart: Python writes -255 as `-ff`, and pads after the sign.
    let signed = |negative: bool, digits: String| (negative, digits);
    let magnitude = |i: i128, digits: fn(u128) -> String| signed(i < 0, digits(i.unsigned_abs()));
    let float = |digits: &dyn Fn(f64) -> String| {
        let v = number()?;
        let text = match v {
            v if v.is_nan() => "nan".to_string(),
            v if v.is_infinite() => "inf".to_string(),
            v => digits(v.abs()),
        };
        let text = if conversion.is_uppercase() { text.to_uppercase() } else { text };
        Some(signed(v.is_sign_negative() && !v.is_nan(), text))
    };
    let precision_or_6 = precision.unwrap_or(6);
    let (negative, digits) = match conversion {
        'd' | 'D' | 'u' => magnitude(int()?, |i| i.to_string()),
        'x' => magnitude(int()?, |i| format!("{i:x}")),
        'X' => magnitude(int()?, |i| format!("{i:X}")),
        'o' | 'O' => magnitude(int()?, |i| format!("{i:o}")),
        'c' | 'C' => return Some(format!("{:<width$}", char::from_u32(u32::try_from(int()?).ok()?)?)),
        'f' | 'F' => float(&|v| format!("{v:.precision_or_6$}"))?,
        'e' | 'E' => float(&|v| exponent(format!("{v:.precision_or_6$e}")))?,
        'g' | 'G' => float(&|v| general(v, precision_or_6, false))?,
        _ => return None,
    };
    let sign = match (negative, plus) {
        (true, _) => "-",
        (false, true) => "+",
        (false, false) => "",
    };
    // Numbers are right-aligned in their width; zeros go between the sign and the digits.
    Some(match zero {
        true => format!("{sign}{digits:0>pad$}", pad = width.saturating_sub(sign.len())),
        false => format!("{:>width$}", format!("{sign}{digits}")),
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
            Type::Array { .. } | Type::Struct { .. } => python_text(&leaves_as_text(self, ty, value, None)),
            _ => plain(ty, value, false),
        }
    }

    /// The members of an array or struct channel value, each leaf as its text: what
    /// `display_channel` writes on one line. `None` for a scalar.
    pub fn display_channel_members(&self, ty: &Type, value: &Value) -> Option<Value> {
        matches!(ty, Type::Array { .. } | Type::Struct { .. }).then(|| leaves_as_text(self, ty, value, None))
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
