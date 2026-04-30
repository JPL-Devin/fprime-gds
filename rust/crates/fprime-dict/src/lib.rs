//! Loader for F´ JSON topology dictionaries.
//!
//! Mirrors the subset of `fprime_gds.common.loaders.*_json_loader` needed to
//! decode events / channels and encode commands.  Only primitive argument
//! types are supported in this initial port — qualified-identifier types
//! (enums, structs, arrays) are currently surfaced as `Raw` blobs so the
//! pipeline keeps working even for richer dictionaries.

#![deny(rust_2018_idioms)]

use std::{collections::HashMap, fs, path::Path};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DictError {
    #[error("io error reading dictionary: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("duplicate {kind} id {id}: {first} vs {second}")]
    Duplicate {
        kind: &'static str,
        id: u32,
        first: String,
        second: String,
    },
    #[error("malformed typeDefinition for {0}")]
    BadTypeDef(String),
    #[error("unknown typeDefinition kind: {0}")]
    UnknownTypeDefKind(String),
}

/// Top-level F´ dictionary as it appears on disk.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDictionary {
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub commands: Vec<RawCommand>,
    #[serde(default)]
    pub events: Vec<RawEvent>,
    #[serde(default)]
    pub telemetry_channels: Vec<RawChannel>,
    #[serde(default)]
    pub type_definitions: Vec<RawTypeDef>,
}

/// `typeDefinitions[]` entry.  We discriminate on `kind` and accept any
/// fields each kind needs.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTypeDef {
    pub kind: String,
    pub qualified_name: String,
    /// Present on `alias`.
    #[serde(default)]
    pub underlying_type: Option<TypeRef>,
    /// Present on `array`.
    #[serde(default)]
    pub element_type: Option<TypeRef>,
    /// Present on `array` (number of elements).
    #[serde(default)]
    pub size: Option<usize>,
    /// Present on `enum`.
    #[serde(default)]
    pub representation_type: Option<TypeRef>,
    /// Present on `enum`.
    #[serde(default)]
    pub enumerated_constants: Vec<RawEnumConstant>,
    /// Present on `struct`.  Map preserves member order *only as JSON gives
    /// it to us* — the actual wire order is dictated by each member's `index`.
    #[serde(default)]
    pub members: HashMap<String, RawStructMember>,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawEnumConstant {
    pub name: String,
    pub value: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStructMember {
    #[serde(rename = "type")]
    pub ty: TypeRef,
    pub index: u32,
    /// When present on a struct member of qualified array type, wraps the
    /// member's declared type into an outer array of this size.  Mirrors
    /// `JsonLoader.construct_serializable_type` in the Python GDS.
    #[serde(default)]
    pub size: Option<usize>,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCommand {
    pub name: String,
    pub opcode: u32,
    #[serde(default)]
    pub formal_params: Vec<FormalParam>,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEvent {
    pub name: String,
    pub id: u32,
    pub severity: String,
    pub format: String,
    #[serde(default)]
    pub formal_params: Vec<FormalParam>,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawChannel {
    pub name: String,
    pub id: u32,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormalParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub annotation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TypeRef {
    pub name: String,
    pub kind: String,
    /// For `integer`/`float`: bit width.  For `string`: maximum length in
    /// bytes (declared at FPP).
    #[serde(default)]
    pub size: Option<u32>,
    #[serde(default)]
    pub signed: Option<bool>,
}

impl TypeRef {
    /// Map this type reference to a primitive name understood by
    /// `fprime_types::Value::deserialize_named`, when possible.
    pub fn primitive_name(&self) -> Option<&'static str> {
        match self.kind.as_str() {
            "integer" => match (self.size, self.signed) {
                (Some(8), Some(false)) => Some("U8"),
                (Some(16), Some(false)) => Some("U16"),
                (Some(32), Some(false)) => Some("U32"),
                (Some(64), Some(false)) => Some("U64"),
                (Some(8), Some(true)) => Some("I8"),
                (Some(16), Some(true)) => Some("I16"),
                (Some(32), Some(true)) => Some("I32"),
                (Some(64), Some(true)) => Some("I64"),
                _ => None,
            },
            "float" => match self.size {
                Some(32) => Some("F32"),
                Some(64) => Some("F64"),
                _ => None,
            },
            "bool" => Some("bool"),
            "string" => Some("string"),
            _ => None,
        }
    }
}

/// In-memory dictionary indexed for fast lookup by id and qualified name.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    pub commands_by_opcode: HashMap<u32, Command>,
    pub commands_by_name: HashMap<String, u32>,
    pub events_by_id: HashMap<u32, Event>,
    pub channels_by_id: HashMap<u32, Channel>,
    /// `typeDefinitions[]` indexed by qualified name (e.g.
    /// `Ref.DpDemo.ColorEnum`, `FwOpcodeType`).  Aliases are kept as-is here;
    /// callers that want to resolve an alias chain should iterate via
    /// [`Dictionary::resolve_alias`].
    pub types_by_name: HashMap<String, TypeDef>,
}

/// A typeDefinition entry, parsed.
#[derive(Debug, Clone)]
pub enum TypeDef {
    /// `kind: "alias"` — a named alias for another type.
    Alias { underlying: TypeRef },
    /// `kind: "array"` — fixed-length array of `element`.
    Array { element: TypeRef, size: usize },
    /// `kind: "enum"` — named integer enum.
    Enum {
        representation: TypeRef,
        constants: Vec<(String, i64)>,
    },
    /// `kind: "struct"` — ordered struct, members listed in wire order
    /// (sorted by their declared `index`).
    Struct { members: Vec<StructMember> },
}

/// A single struct member, in wire order.
#[derive(Debug, Clone)]
pub struct StructMember {
    pub name: String,
    pub ty: TypeRef,
    /// When `Some`, the member is an inline array of `inline_array_size` of
    /// `ty`.  Mirrors how the Python GDS handles a `size` override on a
    /// struct member.
    pub inline_array_size: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Command {
    pub opcode: u32,
    pub name: String,
    pub params: Vec<FormalParam>,
    pub annotation: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: u32,
    pub name: String,
    pub severity: String,
    pub format: String,
    pub params: Vec<FormalParam>,
    pub annotation: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: u32,
    pub name: String,
    pub ty: TypeRef,
    pub annotation: Option<String>,
}

impl Dictionary {
    pub fn from_path(path: &Path) -> Result<Self, DictError> {
        let bytes = fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DictError> {
        let raw: RawDictionary = serde_json::from_slice(bytes)?;
        Self::from_raw(raw)
    }

    pub fn from_raw(raw: RawDictionary) -> Result<Self, DictError> {
        let mut dict = Dictionary::default();

        for c in raw.commands {
            let cmd = Command {
                opcode: c.opcode,
                name: c.name.clone(),
                params: c.formal_params,
                annotation: c.annotation,
            };
            if let Some(existing) = dict.commands_by_opcode.insert(c.opcode, cmd.clone()) {
                return Err(DictError::Duplicate {
                    kind: "command",
                    id: c.opcode,
                    first: existing.name,
                    second: cmd.name,
                });
            }
            dict.commands_by_name.insert(c.name, c.opcode);
        }

        for e in raw.events {
            let ev = Event {
                id: e.id,
                name: e.name.clone(),
                severity: e.severity,
                format: e.format,
                params: e.formal_params,
                annotation: e.annotation,
            };
            if let Some(existing) = dict.events_by_id.insert(e.id, ev.clone()) {
                return Err(DictError::Duplicate {
                    kind: "event",
                    id: e.id,
                    first: existing.name,
                    second: ev.name,
                });
            }
        }

        for td in raw.type_definitions {
            let qn = td.qualified_name.clone();
            let parsed = parse_type_def(td)?;
            dict.types_by_name.insert(qn, parsed);
        }

        for c in raw.telemetry_channels {
            let ch = Channel {
                id: c.id,
                name: c.name.clone(),
                ty: c.ty,
                annotation: c.annotation,
            };
            if let Some(existing) = dict.channels_by_id.insert(c.id, ch.clone()) {
                return Err(DictError::Duplicate {
                    kind: "channel",
                    id: c.id,
                    first: existing.name,
                    second: ch.name,
                });
            }
        }

        Ok(dict)
    }

    pub fn command_by_name(&self, name: &str) -> Option<&Command> {
        self.commands_by_name
            .get(name)
            .and_then(|op| self.commands_by_opcode.get(op))
    }

    /// Walk through any chain of `Alias` typeDefinitions starting at `name`
    /// and return the underlying [`TypeRef`].  Returns `None` if `name` is
    /// not in `types_by_name`.
    pub fn resolve_alias<'a>(&'a self, name: &str) -> Option<&'a TypeRef> {
        let mut current = self.types_by_name.get(name)?;
        loop {
            match current {
                TypeDef::Alias { underlying } => match underlying.kind.as_str() {
                    "qualifiedIdentifier" => match self.types_by_name.get(&underlying.name) {
                        Some(next) => current = next,
                        None => return Some(underlying),
                    },
                    _ => return Some(underlying),
                },
                _ => return None,
            }
        }
    }
}

fn parse_type_def(td: RawTypeDef) -> Result<TypeDef, DictError> {
    match td.kind.as_str() {
        "alias" => Ok(TypeDef::Alias {
            underlying: td
                .underlying_type
                .ok_or_else(|| DictError::BadTypeDef(td.qualified_name.clone()))?,
        }),
        "array" => Ok(TypeDef::Array {
            element: td
                .element_type
                .ok_or_else(|| DictError::BadTypeDef(td.qualified_name.clone()))?,
            size: td
                .size
                .ok_or_else(|| DictError::BadTypeDef(td.qualified_name.clone()))?,
        }),
        "enum" => Ok(TypeDef::Enum {
            representation: td
                .representation_type
                .ok_or_else(|| DictError::BadTypeDef(td.qualified_name.clone()))?,
            constants: td
                .enumerated_constants
                .into_iter()
                .map(|c| (c.name, c.value))
                .collect(),
        }),
        "struct" => {
            let mut members: Vec<(u32, StructMember)> = td
                .members
                .into_iter()
                .map(|(name, m)| {
                    (
                        m.index,
                        StructMember {
                            name,
                            ty: m.ty,
                            inline_array_size: m.size,
                        },
                    )
                })
                .collect();
            members.sort_by_key(|(i, _)| *i);
            Ok(TypeDef::Struct {
                members: members.into_iter().map(|(_, m)| m).collect(),
            })
        }
        other => Err(DictError::UnknownTypeDefKind(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "metadata": {},
      "commands": [
        {"name": "Ref.cmdDisp.CMD_NO_OP", "opcode": 1280, "commandKind": "async", "formalParams": [], "queueFullBehavior": "assert"},
        {"name": "Ref.cmdDisp.CMD_NO_OP_STRING", "opcode": 1281, "commandKind": "async", "formalParams": [
          {"name": "arg1", "type": {"name": "string", "kind": "string", "size": 40}, "ref": false}
        ], "queueFullBehavior": "assert"}
      ],
      "events": [
        {"name": "Ref.evt.E1", "id": 512, "severity": "DIAGNOSTIC", "format": "hi", "formalParams": []}
      ],
      "telemetryChannels": [
        {"name": "Ref.tlm.T1", "id": 256, "type": {"name": "U32", "kind": "integer", "size": 32, "signed": false}, "telemetryUpdate": "always"}
      ]
    }"#;

    #[test]
    fn parse_sample() {
        let dict = Dictionary::from_bytes(SAMPLE.as_bytes()).unwrap();
        assert_eq!(dict.commands_by_opcode.len(), 2);
        assert_eq!(dict.events_by_id.len(), 1);
        assert_eq!(dict.channels_by_id.len(), 1);

        let nop = dict.command_by_name("Ref.cmdDisp.CMD_NO_OP").unwrap();
        assert_eq!(nop.opcode, 1280);
        let s_cmd = dict
            .command_by_name("Ref.cmdDisp.CMD_NO_OP_STRING")
            .unwrap();
        assert_eq!(s_cmd.params[0].ty.primitive_name(), Some("string"));
    }
}
