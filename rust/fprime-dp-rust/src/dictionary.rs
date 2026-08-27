//! F Prime JSON dictionary parsing, mirroring
//! `fprime_gds.common.loaders.json_loader` / `dp_json_loader`.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{FType, IntKind};

/// A data product record definition from the dictionary
#[derive(Debug, Clone)]
pub struct DpRecord {
    pub id: u64,
    pub name: String,
    pub ftype: FType,
    pub is_array: bool,
    pub description: Option<String>,
}

pub struct Dictionary {
    json: Value,
    pub records: BTreeMap<u64, DpRecord>,
}

/// Convert an FPP-style format string to a Python-style format string,
/// mirroring `preprocess_fpp_format_str` (e.g. `{x}` -> `{:x}`)
fn preprocess_format(format: &str) -> String {
    // Pattern: {(\d*\.?\d*[cdxoefgCDXOEFG])}
    let mut out = String::new();
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '}') {
                let inner: String = chars[i + 1..i + 1 + end].iter().collect();
                let valid = !inner.is_empty()
                    && inner
                        .chars()
                        .rev()
                        .skip(1)
                        .all(|c| c.is_ascii_digit() || c == '.')
                    && "cdxoefgCDXOEFG".contains(inner.chars().last().unwrap())
                    && inner.matches('.').count() <= 1;
                if valid {
                    out.push_str(&format!("{{:{}}}", inner));
                    i += end + 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

impl Dictionary {
    pub fn load(path: &str) -> Result<Dictionary, String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("Unable to read dictionary {}: {}", path, e))?;
        let json: Value =
            serde_json::from_str(&data).map_err(|e| format!("Invalid dictionary JSON: {}", e))?;
        let mut dictionary = Dictionary {
            json,
            records: BTreeMap::new(),
        };
        dictionary.parse_records()?;
        Ok(dictionary)
    }

    fn parse_records(&mut self) -> Result<(), String> {
        let records = match self.json.get("records").and_then(|r| r.as_array()) {
            Some(records) => records.clone(),
            None => return Ok(()),
        };
        for record in &records {
            let name = record["name"]
                .as_str()
                .ok_or("Record missing name")?
                .to_string();
            let id = record["id"].as_u64().ok_or("Record missing id")?;
            let ftype = self.parse_type(&record["type"])?;
            let is_array = record["array"].as_bool().unwrap_or(false);
            let description = record["annotation"].as_str().map(|s| s.to_string());
            self.records.insert(
                id,
                DpRecord {
                    id,
                    name,
                    ftype,
                    is_array,
                    description,
                },
            );
        }
        Ok(())
    }

    /// Look up a constant value (e.g. `Fw.DpCfg.CONTAINER_USER_DATA_SIZE`)
    pub fn get_constant(&self, name: &str) -> Result<u64, String> {
        for constant in self
            .json
            .get("constants")
            .and_then(|c| c.as_array())
            .unwrap_or(&vec![])
        {
            if constant["qualifiedName"].as_str() == Some(name) {
                return constant["value"]
                    .as_u64()
                    .ok_or_else(|| format!("Constant {} is not an integer", name));
            }
        }
        Err(format!("Constant {} not found in dictionary", name))
    }

    /// Resolve a type by qualified name from the typeDefinitions section
    pub fn resolve(&self, name: &str) -> Result<FType, String> {
        if let Some(kind) = IntKind::from_name(name) {
            return Ok(FType::Int(kind));
        }
        match name {
            "F32" => return Ok(FType::F32),
            "F64" => return Ok(FType::F64),
            "bool" => return Ok(FType::Bool),
            _ => {}
        }
        let type_defs = self
            .json
            .get("typeDefinitions")
            .and_then(|t| t.as_array())
            .ok_or("Dictionary has no typeDefinitions")?;
        for type_def in type_defs {
            if type_def["qualifiedName"].as_str() == Some(name) {
                return self.parse_type_definition(type_def);
            }
        }
        Err(format!(
            "Dictionary type name has no corresponding type definition: {}",
            name
        ))
    }

    /// Parse a type reference (e.g. a record type), mirroring `parse_type`
    pub fn parse_type(&self, type_dict: &Value) -> Result<FType, String> {
        let name = type_dict["name"]
            .as_str()
            .ok_or_else(|| format!("Dictionary type entry has no name: {}", type_dict))?;
        if name == "string" {
            let size = type_dict["size"]
                .as_u64()
                .ok_or("String type has no size")? as usize;
            return Ok(FType::String(size));
        }
        self.resolve(name)
    }

    /// Parse a typeDefinitions entry, mirroring `parse_type_definition`
    fn parse_type_definition(&self, type_def: &Value) -> Result<FType, String> {
        let name = type_def["qualifiedName"]
            .as_str()
            .ok_or("Type definition has no qualifiedName")?
            .to_string();
        match type_def["kind"].as_str() {
            Some("alias") => self.parse_type(&type_def["underlyingType"]),
            Some("enum") => {
                let rep_name = type_def["representationType"]["name"]
                    .as_str()
                    .ok_or("Enum has no representationType")?;
                let rep = IntKind::from_name(rep_name)
                    .ok_or_else(|| format!("Bad enum representation type {}", rep_name))?;
                let mut members = Vec::new();
                for member in type_def["enumeratedConstants"]
                    .as_array()
                    .ok_or("Enum has no enumeratedConstants")?
                {
                    let key = member["name"]
                        .as_str()
                        .ok_or("Enum constant has no name")?
                        .to_string();
                    let value = member["value"].as_i64().unwrap_or(0);
                    members.push((key, value));
                }
                Ok(FType::Enum(name, rep, members))
            }
            Some("array") => {
                let elem = self.parse_type(&type_def["elementType"])?;
                let size = type_def["size"].as_u64().ok_or("Array has no size")? as usize;
                let format =
                    preprocess_format(type_def["elementType"]["format"].as_str().unwrap_or("{}"));
                Ok(FType::Array(name, Box::new(elem), size, format))
            }
            Some("struct") => {
                let members_obj = type_def["members"]
                    .as_object()
                    .ok_or("Struct has no members")?;
                let mut by_index: BTreeMap<u64, (String, FType, String, String)> = BTreeMap::new();
                for (member_name, member) in members_obj {
                    let mut member_type = self.parse_type(&member["type"])?;
                    // Inline member arrays (declared with a size on the member)
                    if let Some(size) = member["size"].as_u64() {
                        let format =
                            preprocess_format(member["type"]["format"].as_str().unwrap_or("{}"));
                        member_type = FType::Array(
                            format!("Array_{}_{}", member_type.class_name(), size),
                            Box::new(member_type),
                            size as usize,
                            format,
                        );
                    }
                    let format = preprocess_format(member["format"].as_str().unwrap_or("{}"));
                    let description = member["annotation"].as_str().unwrap_or("").to_string();
                    let index = member["index"]
                        .as_u64()
                        .ok_or("Struct member has no index")?;
                    by_index.insert(
                        index,
                        (member_name.clone(), member_type, format, description),
                    );
                }
                Ok(FType::Struct(name, by_index.into_values().collect()))
            }
            _ => Err(format!("Type definition has unknown kind: {}", type_def)),
        }
    }
}
