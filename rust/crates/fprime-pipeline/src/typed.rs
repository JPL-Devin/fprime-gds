//! Dictionary-driven deserializer for compound F´ types.
//!
//! Primitive types are still handled by [`fprime_types::Value::deserialize_named`].
//! Anything whose `kind` is `qualifiedIdentifier` is resolved against the
//! dictionary's `typeDefinitions` here (alias chains, arrays, enums, structs).
//!
//! Wire format follows the FPP/F´ ground-rules used by the Python GDS
//! (`fprime_gds.common.loaders.json_loader.parse_type_definition`):
//!
//! * **alias** \u2014 walks through to the underlying type.
//! * **array** \u2014 fixed-length, items in declaration order, no length prefix.
//! * **enum** \u2014 the underlying integer (signed or unsigned, of `representationType`'s width).
//! * **struct** \u2014 each member, in `index` order; if a member declares a
//!   `size`, that member is wrapped into an inline array of that size.

use fprime_dict::{Dictionary, TypeDef, TypeRef};
use fprime_types::Value;

use crate::PipelineError;

/// Deserialize one [`Value`] of the type described by `ty`, against `dict`.
/// Returns `(value, bytes_consumed)`.
pub fn deserialize_typed(
    ty: &TypeRef,
    dict: &Dictionary,
    data: &[u8],
    offset: usize,
) -> Result<(Value, usize), PipelineError> {
    if let Some(prim) = ty.primitive_name() {
        let (v, n) = Value::deserialize_named(prim, data, offset)?;
        return Ok((v, n));
    }

    match ty.kind.as_str() {
        "qualifiedIdentifier" => {
            let td =
                dict.types_by_name
                    .get(&ty.name)
                    .ok_or_else(|| PipelineError::UnsupportedType {
                        kind: ty.kind.clone(),
                        name: ty.name.clone(),
                    })?;
            deserialize_typedef(&ty.name, td, dict, data, offset)
        }
        _ => Err(PipelineError::UnsupportedType {
            kind: ty.kind.clone(),
            name: ty.name.clone(),
        }),
    }
}

fn deserialize_typedef(
    type_name: &str,
    td: &TypeDef,
    dict: &Dictionary,
    data: &[u8],
    offset: usize,
) -> Result<(Value, usize), PipelineError> {
    match td {
        TypeDef::Alias { underlying } => deserialize_typed(underlying, dict, data, offset),
        TypeDef::Array { element, size } => {
            deserialize_array(type_name, element, *size, dict, data, offset)
        }
        TypeDef::Enum {
            representation,
            constants,
        } => deserialize_enum(type_name, representation, constants, data, offset),
        TypeDef::Struct { members } => {
            let mut cursor = offset;
            let mut fields = Vec::with_capacity(members.len());
            for m in members {
                let (v, n) = if let Some(size) = m.inline_array_size {
                    // Inline-array member: read `size` of `m.ty` directly.
                    deserialize_array(&m.name, &m.ty, size, dict, data, cursor)?
                } else {
                    deserialize_typed(&m.ty, dict, data, cursor)?
                };
                cursor += n;
                fields.push((m.name.clone(), v));
            }
            Ok((
                Value::Struct {
                    type_name: type_name.to_owned(),
                    fields,
                },
                cursor - offset,
            ))
        }
    }
}

fn deserialize_array(
    type_name: &str,
    element: &TypeRef,
    size: usize,
    dict: &Dictionary,
    data: &[u8],
    offset: usize,
) -> Result<(Value, usize), PipelineError> {
    let mut cursor = offset;
    let mut items = Vec::with_capacity(size);
    for _ in 0..size {
        let (v, n) = deserialize_typed(element, dict, data, cursor)?;
        cursor += n;
        items.push(v);
    }
    Ok((
        Value::Array {
            type_name: type_name.to_owned(),
            items,
        },
        cursor - offset,
    ))
}

fn deserialize_enum(
    type_name: &str,
    representation: &TypeRef,
    constants: &[(String, i64)],
    data: &[u8],
    offset: usize,
) -> Result<(Value, usize), PipelineError> {
    let prim = representation
        .primitive_name()
        .ok_or_else(|| PipelineError::UnsupportedType {
            kind: representation.kind.clone(),
            name: representation.name.clone(),
        })?;
    let (v, n) = Value::deserialize_named(prim, data, offset)?;
    let raw: i64 = match v {
        Value::U8(x) => x as i64,
        Value::U16(x) => x as i64,
        Value::U32(x) => x as i64,
        Value::U64(x) => x as i64, // truncating on overflow is acceptable for label lookup
        Value::I8(x) => x as i64,
        Value::I16(x) => x as i64,
        Value::I32(x) => x as i64,
        Value::I64(x) => x,
        _ => {
            return Err(PipelineError::UnsupportedType {
                kind: representation.kind.clone(),
                name: representation.name.clone(),
            })
        }
    };
    let label = constants
        .iter()
        .find(|(_, val)| *val == raw)
        .map(|(name, _)| name.clone());
    Ok((
        Value::Enum {
            type_name: type_name.to_owned(),
            label,
            value: raw,
        },
        n,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fprime_dict::{Dictionary, StructMember, TypeDef, TypeRef};

    fn int(name: &'static str, size: u32, signed: bool) -> TypeRef {
        TypeRef {
            name: name.into(),
            kind: "integer".into(),
            size: Some(size),
            signed: Some(signed),
        }
    }

    fn qref(name: &'static str) -> TypeRef {
        TypeRef {
            name: name.into(),
            kind: "qualifiedIdentifier".into(),
            size: None,
            signed: None,
        }
    }

    #[test]
    fn enum_decodes_label() {
        let mut dict = Dictionary::default();
        dict.types_by_name.insert(
            "Color".into(),
            TypeDef::Enum {
                representation: int("U8", 8, false),
                constants: vec![("RED".into(), 0), ("GREEN".into(), 1), ("BLUE".into(), 2)],
            },
        );
        let (v, n) = deserialize_typed(&qref("Color"), &dict, &[0x01], 0).unwrap();
        assert_eq!(n, 1);
        match v {
            Value::Enum { label, value, .. } => {
                assert_eq!(label.as_deref(), Some("GREEN"));
                assert_eq!(value, 1);
            }
            _ => panic!("expected enum, got {v:?}"),
        }
    }

    #[test]
    fn array_decodes_items() {
        let mut dict = Dictionary::default();
        dict.types_by_name.insert(
            "Triple".into(),
            TypeDef::Array {
                element: int("U16", 16, false),
                size: 3,
            },
        );
        let bytes = [0x00, 0x01, 0x00, 0x02, 0x00, 0x03];
        let (v, n) = deserialize_typed(&qref("Triple"), &dict, &bytes, 0).unwrap();
        assert_eq!(n, 6);
        match v {
            Value::Array { items, .. } => {
                assert_eq!(items.len(), 3);
                assert!(matches!(items[0], Value::U16(1)));
                assert!(matches!(items[2], Value::U16(3)));
            }
            _ => panic!("expected array, got {v:?}"),
        }
    }

    #[test]
    fn alias_resolves_through() {
        let mut dict = Dictionary::default();
        dict.types_by_name.insert(
            "FwOpcodeType".into(),
            TypeDef::Alias {
                underlying: int("U32", 32, false),
            },
        );
        let bytes = [0x01, 0x00, 0x00, 0x00];
        let (v, n) = deserialize_typed(&qref("FwOpcodeType"), &dict, &bytes, 0).unwrap();
        assert_eq!(n, 4);
        assert!(matches!(v, Value::U32(0x0100_0000)));
    }

    #[test]
    fn struct_decodes_members_in_index_order() {
        let mut dict = Dictionary::default();
        dict.types_by_name.insert(
            "Pair".into(),
            TypeDef::Struct {
                members: vec![
                    StructMember {
                        name: "first".into(),
                        ty: int("U16", 16, false),
                        inline_array_size: None,
                    },
                    StructMember {
                        name: "second".into(),
                        ty: int("U8", 8, false),
                        inline_array_size: None,
                    },
                ],
            },
        );
        let bytes = [0x00, 0x42, 0x07];
        let (v, n) = deserialize_typed(&qref("Pair"), &dict, &bytes, 0).unwrap();
        assert_eq!(n, 3);
        match v {
            Value::Struct { fields, .. } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "first");
                assert!(matches!(fields[0].1, Value::U16(0x42)));
                assert_eq!(fields[1].0, "second");
                assert!(matches!(fields[1].1, Value::U8(7)));
            }
            _ => panic!("expected struct, got {v:?}"),
        }
    }

    #[test]
    fn struct_inline_array_member_uses_size_override() {
        let mut dict = Dictionary::default();
        dict.types_by_name.insert(
            "Pair".into(),
            TypeDef::Struct {
                members: vec![StructMember {
                    name: "ids".into(),
                    ty: int("U8", 8, false),
                    inline_array_size: Some(3),
                }],
            },
        );
        let bytes = [0x01, 0x02, 0x03];
        let (v, _) = deserialize_typed(&qref("Pair"), &dict, &bytes, 0).unwrap();
        match v {
            Value::Struct { fields, .. } => match &fields[0].1 {
                Value::Array { items, .. } => assert_eq!(items.len(), 3),
                other => panic!("expected array, got {other:?}"),
            },
            _ => panic!("expected struct"),
        }
    }
}
