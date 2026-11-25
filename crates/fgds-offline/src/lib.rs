//! What the offline tools share (sequence compiler, parameter tools): the values of their
//! input files as the Python tools read them, the conversion of a value to a dictionary
//! type, the dictionary file as those tools accept it, and the checksum of their output.
//! A library, not a plugin: plugins may not depend on each other (MNT-1).

pub mod convert;
pub mod value;

use fgds_core::serde_json::{self, json};
use fgds_core::{CliEnv, Dictionary, Error, Result};
use std::path::{Path, PathBuf};

/// Largest sequence or dictionary file read (LNK-9).
pub const MAX_INPUT: u64 = 256 * 1024 * 1024;

const CRC32: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// The checksum of sequence and parameter files (`zlib.crc32`).
pub fn crc32(bytes: &[u8]) -> u32 {
    CRC32.checksum(bytes)
}

/// JSON on one line with `, ` and `: ` between the parts and ASCII only: what a sequence carries in a
/// string argument, so that the bytes sent to flight stay those of `fprime-seqgen`.
pub fn json_line(value: &serde_json::Value) -> String {
    value::Value::from_json(value).to_json()
}

/// Read a file of at most `MAX_INPUT` bytes.
pub fn read_file(path: &Path) -> std::result::Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|_| format!("Can't open file '{}'. ", path.display()))?;
    if !meta.is_file() {
        return Err(format!("Can't open file '{}'. ", path.display()));
    }
    if meta.len() > MAX_INPUT {
        return Err(format!("{}: larger than {MAX_INPUT} bytes", path.display()));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The JSON of a dictionary file for the offline tools. The sections a full dictionary has
/// and these tools do not use may be absent, as the Python tools allow.
pub fn read_dictionary_json(path: &Path) -> std::result::Result<serde_json::Value, String> {
    let bytes = read_file(path)?;
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(top) = json.as_object_mut() {
        for section in ["typeDefinitions", "commands", "events", "telemetryChannels"] {
            top.entry(section).or_insert_with(|| json!([]));
        }
    }
    complete_types(&mut json, false);
    Ok(json)
}

/// Load a dictionary for the offline tools (see `read_dictionary_json`).
pub fn load_dictionary(path: &Path) -> std::result::Result<Dictionary, String> {
    let json = read_dictionary_json(path)?;
    Dictionary::parse(&path.display().to_string(), &json.to_string()).map_err(|e| e.to_string())
}

/// A type reference may have its name alone (`fixtures/sequences/resources/enhanced_dictionary.json`
/// has such): give it the fields the dictionary loader reads.
fn complete_types(json: &mut serde_json::Value, reference: bool) {
    match json {
        serde_json::Value::Array(items) => items.iter_mut().for_each(|item| complete_types(item, false)),
        serde_json::Value::Object(map) => {
            if reference {
                let name = map.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                let number = |prefix: char| name.strip_prefix(prefix).and_then(|bits| bits.parse::<u8>().ok());
                let fields = match (name.as_str(), number('I'), number('U'), number('F')) {
                    ("bool", ..) => json!({"kind": "bool"}),
                    ("string", ..) => json!({"kind": "string"}),
                    (_, Some(bits), ..) => json!({"kind": "integer", "size": bits, "signed": true}),
                    (_, _, Some(bits), _) => json!({"kind": "integer", "size": bits, "signed": false}),
                    (_, _, _, Some(bits)) => json!({"kind": "float", "size": bits}),
                    _ => json!({"kind": "qualifiedIdentifier"}),
                };
                for (key, value) in fields.as_object().into_iter().flatten() {
                    map.entry(key.clone()).or_insert(value.clone());
                }
            }
            for (key, value) in map.iter_mut() {
                let reference =
                    matches!(key.as_str(), "type" | "representationType" | "elementType" | "underlyingType");
                complete_types(value, reference);
            }
        }
        _ => {}
    }
}

/// The dictionary named on the command line or in the configuration.
pub fn dictionary_path(env: &CliEnv) -> Result<PathBuf> {
    if env.config.dictionary == Path::new("") {
        return Err(Error::Config("no dictionary: give --dictionary or set `dictionary` in the configuration".into()));
    }
    Ok(env.config.dictionary.clone())
}
