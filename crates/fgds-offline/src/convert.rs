//! From a value written in a sequence or a parameter file to the bytes of a dictionary type,
//! accepting and rejecting what the Python GDS accepts and rejects, with its messages.

use crate::value::{parse_float, parse_int, str_repr, Int, Value};
use fgds_core::dictionary::Type;
use fgds_core::serde_json::json;
use fgds_core::Dictionary;

/// Where the value comes from; the two Python tools differ in a few conversions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    /// An argument of a command in a sequence file: arrays and structs may be JSON text.
    Argument,
    /// A value of a parameter JSON file.
    Parameter,
}

/// The name the Python GDS gives the class of a type.
pub fn type_name(ty: &Type) -> String {
    match ty {
        Type::Int { bits, signed } => format!("{}{bits}Type", if *signed { 'I' } else { 'U' }),
        Type::Float { bits } => format!("F{bits}Type"),
        Type::Bool => "BoolType".into(),
        Type::String { max } => format!("String_{max}"),
        Type::Enum { name, .. } | Type::Array { name, .. } | Type::Struct { name, .. } => name.clone(),
    }
}

fn class_repr(ty: &Type) -> String {
    let module = match ty {
        Type::Int { .. } | Type::Float { .. } => "numerical_types",
        Type::Bool => "bool_type",
        _ => "type_base",
    };
    format!("<class 'fprime_gds.common.models.serialize.{module}.{}'>", type_name(ty))
}

fn mismatch<T>(expected: &str, found: &Value) -> Result<T, String> {
    Err(format!("Type <class '{expected}'> expected, type <class '{}'> found!", found.python_type()))
}

fn int_of(value: &Value) -> Option<Int> {
    match value {
        Value::Int(i) => Some(i.clone()),
        // In Python a bool is an int.
        Value::Bool(b) => Some(i128::from(*b).into()),
        _ => None,
    }
}

fn check_int(bits: u8, signed: bool, value: &Int, shown: &str) -> Result<i128, String> {
    let (min, max) = match signed {
        true => (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1),
        false => (0, (1i128 << bits) - 1),
    };
    value.to_i128().filter(|v| (min..=max).contains(v)).ok_or_else(|| format!("Value {shown} out of range!"))
}

fn check_array(size: usize, element: &Type, value: &Value) -> Result<(), String> {
    let Value::Array(items) = value else { return mismatch("list", value) };
    if items.len() != size {
        let class = class_repr(element);
        return Err(format!("Array type {class} is of length {size}, actual length {} found!", items.len()));
    }
    items.iter().try_for_each(|item| validate(element, item))
}

/// The check the Python types make when a value is set.
fn validate(ty: &Type, value: &Value) -> Result<(), String> {
    match ty {
        Type::Int { bits, signed } => match int_of(value) {
            Some(i) => check_int(*bits, *signed, &i, &value.python_str()).map(|_| ()),
            None => mismatch("int", value),
        },
        Type::Float { .. } => match value {
            Value::Float(_) | Value::Int(_) | Value::Bool(_) => Ok(()),
            _ => mismatch("float", value),
        },
        Type::Bool => match value {
            Value::Bool(_) => Ok(()),
            _ => mismatch("bool", value),
        },
        Type::String { max } => match value {
            Value::Str(s) if s.chars().count() > *max => {
                Err(format!("String size {} is greater than {max}!", s.chars().count()))
            }
            Value::Str(_) => Ok(()),
            _ => mismatch("str", value),
        },
        Type::Enum { constants, .. } => match value {
            Value::Str(s) if constants.iter().any(|(name, _)| name == s) => Ok(()),
            Value::Str(s) => Err(format!("Invalid enum member {s} set in ABCMeta enum!")),
            _ => mismatch("str", value),
        },
        Type::Array { size, element, .. } => check_array(*size, element, value),
        Type::Struct { members, .. } => {
            let Value::Object(pairs) = value else { return mismatch("dict", value) };
            if pairs.len() != members.len() {
                let names: Vec<String> = members.iter().map(|m| str_repr(&m.name)).collect();
                return Err(format!("Value does not define required fields: [{}]", names.join(", ")));
            }
            for member in members {
                let Some((_, v)) = pairs.iter().find(|(key, _)| *key == member.name) else {
                    return Err(format!("Value does not define required field: {}", member.name));
                };
                match member.count {
                    Some(count) => check_array(count, &member.ty, v)?,
                    None => validate(&member.ty, v)?,
                }
            }
            Ok(())
        }
    }
}

fn put(dict: &Dictionary, ty: &Type, value: fgds_core::serde_json::Value, out: &mut Vec<u8>) -> Result<(), String> {
    dict.encode(ty, &value, out).map_err(|e| e.0)
}

/// Serialize a value that passed `validate`.
fn encode(dict: &Dictionary, ty: &Type, value: &Value, out: &mut Vec<u8>) -> Result<(), String> {
    let invalid = || format!("{} cannot be written as {}", value.python_str(), type_name(ty));
    match ty {
        Type::Int { bits, signed } => {
            let v = int_of(value).and_then(|i| check_int(*bits, *signed, &i, "").ok()).ok_or_else(invalid)?;
            out.extend_from_slice(&v.to_be_bytes()[16 - usize::from(*bits / 8)..]);
        }
        Type::Float { bits } => {
            let v = match value {
                Value::Float(f) => *f,
                other => int_of(other).and_then(|i| i.to_f64()).ok_or("int too large to convert to float")?,
            };
            if *bits == 32 {
                // Python refuses a finite value that F32 cannot hold.
                if v.is_finite() && !(v as f32).is_finite() {
                    return Err(format!("Value {} out of range!", fgds_core::dictionary::float_text(v)));
                }
                out.extend_from_slice(&(v as f32).to_be_bytes());
            } else {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Type::Bool => match value {
            Value::Bool(b) => put(dict, ty, json!(b), out)?,
            _ => return Err(invalid()),
        },
        Type::String { .. } => {
            let Value::Str(s) = value else { return Err(invalid()) };
            let prefix = dict.config_type("FwSizeStoreType", Type::U16);
            put(dict, &prefix, json!(s.len()), out)
                .map_err(|_| format!("String of {} bytes is too long for its size prefix", s.len()))?;
            out.extend_from_slice(s.as_bytes());
        }
        Type::Enum { rep, constants, .. } => {
            let Value::Str(s) = value else { return Err(invalid()) };
            let number = constants.iter().find(|(name, _)| name == s).ok_or_else(invalid)?.1;
            put(dict, rep, json!(number), out)?;
        }
        Type::Array { element, .. } => {
            let Value::Array(items) = value else { return Err(invalid()) };
            items.iter().try_for_each(|item| encode(dict, element, item, out))?;
        }
        Type::Struct { members, .. } => {
            let Value::Object(pairs) = value else { return Err(invalid()) };
            for member in members {
                let v = &pairs.iter().find(|(key, _)| *key == member.name).ok_or_else(invalid)?.1;
                match (member.count, v) {
                    (Some(_), Value::Array(items)) => {
                        items.iter().try_for_each(|item| encode(dict, &member.ty, item, out))?;
                    }
                    (Some(_), _) => return Err(invalid()),
                    (None, v) => encode(dict, &member.ty, v, out)?,
                }
            }
        }
    }
    Ok(())
}

/// Python's `int(value, 0)` for text and `int(value)` otherwise.
fn to_int(value: &Value) -> Result<Int, String> {
    match value {
        Value::Str(s) => parse_int(s).ok_or_else(|| format!("invalid literal for int() with base 0: {}", str_repr(s))),
        Value::Float(f) if f.is_nan() => Err("cannot convert float NaN to integer".into()),
        Value::Float(f) if f.is_infinite() => Err("cannot convert float infinity to integer".into()),
        Value::Float(f) => Int::parse(&format!("{:.0}", f.trunc()), 10).ok_or_else(|| "not an integer".into()),
        other => int_of(other).ok_or_else(|| {
            let name = other.python_type();
            format!("int() argument must be a string, a bytes-like object or a real number, not '{name}'")
        }),
    }
}

/// Python's `float(value)`.
fn to_float(value: &Value) -> Result<f64, String> {
    match value {
        Value::Str(s) => parse_float(s).ok_or_else(|| format!("could not convert string to float: {}", str_repr(s))),
        Value::Float(f) => Ok(*f),
        other => match int_of(other) {
            Some(i) => i.to_f64().ok_or_else(|| "int too large to convert to float".into()),
            None => Err(format!("float() argument must be a string or a real number, not '{}'", other.python_type())),
        },
    }
}

/// Convert `value` to `ty` as the Python tool does for a top-level value, and serialize it.
pub fn convert(dict: &Dictionary, ty: &Type, value: &Value, source: Source, out: &mut Vec<u8>) -> Result<(), String> {
    // A sequence hands arrays and objects on as JSON text.
    let text;
    let value = match (source, value, ty) {
        (Source::Argument, Value::Array(_) | Value::Object(_), Type::Array { .. } | Type::Struct { .. }) => value,
        (Source::Argument, Value::Array(_) | Value::Object(_), _) => {
            text = Value::Str(value.to_json());
            &text
        }
        _ => value,
    };
    let converted = match ty {
        Type::Bool => {
            let shown = match (source, value) {
                (Source::Parameter, Value::Array(_) | Value::Object(_)) => String::new(),
                _ => value.python_str(),
            };
            match shown.to_lowercase().trim() {
                "true" | "yes" => Value::Bool(true),
                "false" | "no" => Value::Bool(false),
                _ if source == Source::Argument => return Err("Argument value is not a valid boolean".into()),
                _ => return Err("Param value is not a valid boolean".into()),
            }
        }
        Type::Float { .. } => Value::Float(to_float(value)?),
        Type::Int { .. } => Value::Int(to_int(value)?),
        Type::Array { .. } | Type::Struct { .. } if source == Source::Argument => match value {
            Value::Str(s) => Value::parse_json(s)?,
            Value::Array(_) | Value::Object(_) => value.clone(),
            other => {
                return Err(format!("the JSON object must be str, bytes or bytearray, not {}", other.python_type()))
            }
        },
        _ => value.clone(),
    };
    validate(ty, &converted)?;
    encode(dict, ty, &converted, out)
}
