use crate::types::{str_field, u64_field, Channel, Command, Event, Member, Param, Type};
use crate::Dictionary;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// A dictionary that cannot be used. The message names the file and the first problem (LNK-2).
#[derive(Debug, thiserror::Error)]
#[error("dictionary {file}: {problem}")]
pub struct DictError {
    pub file: String,
    pub problem: String,
}

impl Dictionary {
    pub fn load(path: &Path) -> Result<Dictionary, DictError> {
        let file = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| DictError {
            file: file.clone(),
            problem: e.to_string(),
        })?;
        Self::parse(&file, &text)
    }

    /// Parse dictionary text; `file` is used in error messages only.
    pub fn parse(file: &str, text: &str) -> Result<Dictionary, DictError> {
        let err = |problem: String| DictError {
            file: file.to_string(),
            problem,
        };
        let json: Value = serde_json::from_str(text).map_err(|e| err(e.to_string()))?;
        if !json.is_object() {
            return Err(err("top level is not an object".into()));
        }
        let resolver = Resolver::new(&json).map_err(&err)?;
        let mut types = HashMap::new();
        for name in resolver.defs.keys() {
            let ty = resolver
                .named(name, &mut Vec::new())
                .map_err(|e| err(format!("type {name}: {e}")))?;
            types.insert(name.to_string(), ty);
        }

        let params = |item: &Value| -> Result<Vec<Param>, String> {
            list(item, "formalParams")?
                .iter()
                .map(|p| {
                    Ok(Param {
                        name: str_field(p, "name")?.to_string(),
                        ty: resolver.resolve(&p["type"], &mut Vec::new())?,
                    })
                })
                .collect()
        };
        let id32 = |item: &Value, key: &str| -> Result<u32, String> {
            u32::try_from(u64_field(item, key)?).map_err(|_| format!("\"{key}\" exceeds 32 bits"))
        };

        let mut dict = Dictionary {
            commands: Vec::new(),
            events: Vec::new(),
            channels: Vec::new(),
            command_by_name: HashMap::new(),
            command_by_opcode: HashMap::new(),
            event_by_id: HashMap::new(),
            channel_by_id: HashMap::new(),
            packets: HashMap::new(),
            bool_true: constant(&json, "FW_SERIALIZE_TRUE_VALUE", 0xFF).map_err(&err)?,
            bool_false: constant(&json, "FW_SERIALIZE_FALSE_VALUE", 0x00).map_err(&err)?,
            size_store: types
                .get("FwSizeStoreType")
                .cloned()
                .unwrap_or(Type::U16),
            types,
            json: Value::Null,
        };
        if !matches!(dict.size_store, Type::Int { signed: false, .. }) {
            return Err(err("FwSizeStoreType is not an unsigned integer".into()));
        }

        for (i, item) in list(&json, "commands").map_err(&err)?.iter().enumerate() {
            let at = |e: String| err(format!("commands[{i}]: {e}"));
            let cmd = Command {
                name: str_field(item, "name").map_err(at)?.to_string(),
                opcode: id32(item, "opcode").map_err(at)?,
                params: params(item).map_err(at)?,
            };
            if dict.command_by_name.insert(cmd.name.clone(), i).is_some()
                || dict.command_by_opcode.insert(cmd.opcode, i).is_some()
            {
                return Err(at(format!("duplicate command {}", cmd.name)));
            }
            dict.commands.push(cmd);
        }
        for (i, item) in list(&json, "events").map_err(&err)?.iter().enumerate() {
            let at = |e: String| err(format!("events[{i}]: {e}"));
            let event = Event {
                name: str_field(item, "name").map_err(at)?.to_string(),
                id: id32(item, "id").map_err(at)?,
                severity: str_field(item, "severity").map_err(at)?.to_string(),
                format: str_field(item, "format").map_err(at)?.to_string(),
                params: params(item).map_err(at)?,
            };
            if dict.event_by_id.insert(event.id, i).is_some() {
                return Err(at(format!("duplicate event id {}", event.id)));
            }
            dict.events.push(event);
        }
        for (i, item) in list(&json, "telemetryChannels")
            .map_err(&err)?
            .iter()
            .enumerate()
        {
            let at = |e: String| err(format!("telemetryChannels[{i}]: {e}"));
            let channel = Channel {
                name: str_field(item, "name").map_err(at)?.to_string(),
                id: id32(item, "id").map_err(at)?,
                ty: resolver
                    .resolve(&item["type"], &mut Vec::new())
                    .map_err(at)?,
                format: item["format"].as_str().map(str::to_string),
            };
            if dict.channel_by_id.insert(channel.id, i).is_some() {
                return Err(at(format!("duplicate channel id {}", channel.id)));
            }
            dict.channels.push(channel);
        }
        // Telemetry packets are optional; the first packet set is the one in use.
        let no_packets = Vec::new();
        let packets = json["telemetryPacketSets"][0]["members"].as_array().unwrap_or(&no_packets);
        for (i, item) in packets.iter().enumerate() {
            let at = |e: String| err(format!("telemetryPacketSets[0].members[{i}]: {e}"));
            let mut members = Vec::new();
            for name in list(item, "members").map_err(at)? {
                let index = dict.channels.iter().position(|c| Some(c.name.as_str()) == name.as_str());
                members.push(index.ok_or_else(|| at(format!("unknown channel {name}")))?);
            }
            if dict.packets.insert(id32(item, "id").map_err(at)?, members).is_some() {
                return Err(at("duplicate packet id".into()));
            }
        }
        dict.json = json;
        Ok(dict)
    }
}

/// A required top-level array; absent sections are an error so a wrong file is caught early.
fn list<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    v[key]
        .as_array()
        .ok_or_else(|| format!("missing array \"{key}\""))
}

fn constant(json: &Value, name: &str, default: u8) -> Result<u8, String> {
    let found = json["constants"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["qualifiedName"] == name));
    match found {
        None => Ok(default),
        Some(c) => c["value"]
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(|| format!("constant {name} is not a byte value")),
    }
}

/// Expands type references into `Type` trees, refusing a type that contains itself.
struct Resolver<'a> {
    defs: HashMap<&'a str, &'a Value>,
}

impl<'a> Resolver<'a> {
    fn new(json: &'a Value) -> Result<Self, String> {
        let mut defs = HashMap::new();
        for (i, def) in list(json, "typeDefinitions")?.iter().enumerate() {
            let name =
                str_field(def, "qualifiedName").map_err(|e| format!("typeDefinitions[{i}]: {e}"))?;
            if defs.insert(name, def).is_some() {
                return Err(format!("typeDefinitions[{i}]: duplicate type {name}"));
            }
        }
        Ok(Resolver { defs })
    }

    /// Resolve a type reference: `{"kind": "integer", ...}` and the like.
    fn resolve(&self, r: &'a Value, stack: &mut Vec<&'a str>) -> Result<Type, String> {
        let bits = |allowed: &[u64]| -> Result<u8, String> {
            let size = u64_field(r, "size")?;
            if allowed.contains(&size) {
                Ok(size as u8)
            } else {
                Err(format!("unsupported size {size}"))
            }
        };
        match str_field(r, "kind")? {
            "integer" => Ok(Type::Int {
                bits: bits(&[8, 16, 32, 64])?,
                signed: r["signed"]
                    .as_bool()
                    .ok_or("missing boolean field \"signed\"")?,
            }),
            "float" => Ok(Type::Float {
                bits: bits(&[32, 64])?,
            }),
            "bool" => Ok(Type::Bool),
            "string" => Ok(Type::String {
                max: u64_field(r, "size")? as usize,
            }),
            "qualifiedIdentifier" => self.named(str_field(r, "name")?, stack),
            other => Err(format!("unknown type kind \"{other}\"")),
        }
    }

    fn named(&self, name: &'a str, stack: &mut Vec<&'a str>) -> Result<Type, String> {
        let (name, def) = self
            .defs
            .get_key_value(name)
            .map(|(k, v)| (*k, *v))
            .ok_or_else(|| format!("undefined type {name}"))?;
        if stack.contains(&name) {
            return Err(format!("type {name} contains itself"));
        }
        stack.push(name);
        let ty = self
            .definition(name, def, stack)
            .map_err(|e| match stack.len() {
                1 => e,
                _ => format!("in {name}: {e}"),
            });
        stack.pop();
        ty
    }

    fn definition(
        &self,
        name: &'a str,
        def: &'a Value,
        stack: &mut Vec<&'a str>,
    ) -> Result<Type, String> {
        match str_field(def, "kind")? {
            "alias" => self.resolve(&def["type"], stack),
            "enum" => {
                let rep = self.resolve(&def["representationType"], stack)?;
                if !matches!(rep, Type::Int { .. }) {
                    return Err("enum representation is not an integer".into());
                }
                let constants = list(def, "enumeratedConstants")?
                    .iter()
                    .map(|c| {
                        let value = c["value"].as_i64().ok_or("enum constant without value")?;
                        Ok((str_field(c, "name")?.to_string(), value))
                    })
                    .collect::<Result<_, String>>()?;
                Ok(Type::Enum {
                    name: name.to_string(),
                    rep: Box::new(rep),
                    constants,
                })
            }
            "array" => Ok(Type::Array {
                name: name.to_string(),
                size: u64_field(def, "size")? as usize,
                element: Box::new(self.resolve(&def["elementType"], stack)?),
            }),
            "struct" => {
                let map = def["members"].as_object().ok_or("missing \"members\"")?;
                let mut members = Vec::new();
                for (member, spec) in map {
                    let index = u64_field(spec, "index")?;
                    let ty = self
                        .resolve(&spec["type"], stack)
                        .map_err(|e| format!("member {member}: {e}"))?;
                    let count = spec["size"].as_u64().map(|n| n as usize);
                    members.push((
                        index,
                        Member {
                            name: member.clone(),
                            ty,
                            count,
                        },
                    ));
                }
                // Wire order is the "index" field, not the order of the JSON object.
                members.sort_by_key(|(index, _)| *index);
                Ok(Type::Struct {
                    name: name.to_string(),
                    members: members.into_iter().map(|(_, m)| m).collect(),
                })
            }
            other => Err(format!("unknown type definition kind \"{other}\"")),
        }
    }
}
