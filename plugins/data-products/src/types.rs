//! What the data product tools need from a dictionary: its types (`fgds-dictionary`), the records
//! and containers, and the header of a product, which the dictionary does not describe.

use fgds_core::dictionary::{Member, Type};
use fgds_core::serde_json::{json, Value};
use fgds_core::Dictionary;
use std::collections::HashMap;

/// One record definition of the dictionary.
#[derive(Debug)]
pub struct RecordDef {
    pub id: u64,
    pub name: String,
    pub ty: Type,
    pub is_array: bool,
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct ContainerDef {
    pub id: u64,
    pub name: String,
    pub default_priority: u64,
    pub description: Option<String>,
}

pub struct DpDictionary {
    pub dict: Dictionary,
    pub records: HashMap<u64, RecordDef>,
    pub containers: HashMap<u64, ContainerDef>,
    /// The header of a product, or why this dictionary does not give one.
    header: Result<Type, String>,
}

impl DpDictionary {
    pub fn load(path: &str) -> Result<DpDictionary, String> {
        let text = std::fs::read_to_string(path).map_err(|e| crate::io_error(&e, path))?;
        let json: Value = fgds_core::serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        DpDictionary::from_json(&json)
    }

    pub fn from_json(json: &Value) -> Result<DpDictionary, String> {
        // A dictionary of data products alone has no commands, events or channels.
        let mut whole = json.clone();
        if let Some(top) = whole.as_object_mut() {
            for section in ["typeDefinitions", "commands", "events", "telemetryChannels"] {
                top.entry(section).or_insert_with(|| json!([]));
            }
        }
        let dict = Dictionary::parse("dictionary", &whole.to_string()).map_err(|e| e.to_string())?;
        let list = |key: &str| json[key].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let text = |v: &Value| v.as_str().map(str::to_string);
        let mut records = HashMap::new();
        for record in list("records") {
            let bad = |e: &str| format!("record {}: {e}", record["name"]);
            let id = record["id"].as_u64().ok_or_else(|| bad("no usable id"))?;
            let def = RecordDef {
                id,
                name: text(&record["name"]).ok_or_else(|| bad("no name"))?,
                ty: dict.type_of(&record["type"]).map_err(|e| bad(&e))?,
                is_array: record["array"].as_bool().unwrap_or(false),
                description: text(&record["annotation"]),
            };
            records.insert(id, def);
        }
        let mut containers = HashMap::new();
        for container in list("containers") {
            let bad = |e: &str| format!("container {}: {e}", container["name"]);
            let id = container["id"].as_u64().ok_or_else(|| bad("no usable id"))?;
            let def = ContainerDef {
                id,
                name: text(&container["name"]).ok_or_else(|| bad("no name"))?,
                default_priority: container["defaultPriority"].as_u64().ok_or_else(|| bad("no usable priority"))?,
                description: text(&container["annotation"]),
            };
            containers.insert(id, def);
        }
        let header = header_type(&dict);
        Ok(DpDictionary { dict, records, containers, header })
    }

    /// The header of a product (Fw/Dp SDD), a struct.
    pub fn header_type(&self) -> Result<&Type, String> {
        self.header.as_ref().map_err(String::clone)
    }

    /// The size of the header in bytes.
    pub fn header_size(&self) -> Result<usize, String> {
        Ok(self.dict.max_size(self.header_type()?))
    }

    /// The type that stores the number of elements of an array record.
    pub fn size_type(&self) -> Type {
        self.dict.config_type("FwSizeStoreType", Type::U16)
    }
}

fn header_type(dict: &Dictionary) -> Result<Type, String> {
    let named = |name: &str| dict.types.get(name).cloned().ok_or_else(|| format!("the dictionary has no type {name}"));
    let representation = |name: &str, default: Option<Type>| match (dict.types.get(name), default) {
        (Some(Type::Enum { rep, .. }), _) => Ok((**rep).clone()),
        (Some(_), _) => Err(format!("{name} is not an enum")),
        (None, Some(default)) => Ok(default),
        (None, None) => Err(format!("the dictionary has no type {name}")),
    };
    let constants = dict.json["constants"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let user_data = "Fw.DpCfg.CONTAINER_USER_DATA_SIZE";
    let user_data_size = constants
        .iter()
        .find(|c| c["qualifiedName"] == user_data)
        .and_then(|c| c["value"].as_u64())
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| format!("the dictionary has no constant {user_data}"))?;
    let member = |name: &str, ty: Type| Member { name: name.into(), ty, count: None, format: None, description: None };
    let time = Type::Struct {
        name: "Fw.Time".into(),
        members: vec![
            member("base", representation("TimeBase", Some(Type::U16))?),
            member("context", dict.config_type("FwTimeContextStoreType", Type::U8)),
            member("seconds", Type::U32),
            member("microseconds", Type::U32),
        ],
    };
    let mut user = member("UserData", Type::U8);
    user.count = Some(user_data_size);
    Ok(Type::Struct {
        name: "Fw.DpContainer.Header".into(),
        members: vec![
            member("PacketDescriptor", dict.config_type("FwPacketDescriptorType", Type::U16)),
            member("Id", named("FwDpIdType")?),
            member("Priority", named("FwDpPriorityType")?),
            member("Time", time),
            member("ProcTypes", representation("Fw.DpCfg.ProcType", None)?),
            user,
            member("DpState", named("Fw.DpState")?),
            member("DataSize", dict.config_type("FwSizeStoreType", Type::U16)),
            member("Checksum", Type::U32),
        ],
    })
}
