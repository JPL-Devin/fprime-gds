//! Parameter database files (PRM-1, CLI-5): JSON to the `.dat` file `Svc::PrmDb` loads or to a
//! command sequence, and a `.dat` file back to JSON, text or CSV. The functions of
//! `fprime-prm-write` and `fprime-prm-decode`, as a library and as `fgds` subcommands.

mod decode;

use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::dictionary::Type;
use fgds_core::serde_json::Value;
use fgds_core::serde_json::{self, json};
use fgds_core::{CliEnv, Dictionary, Manifest, Plugin, Registry, Result, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

const DELIMITER: u8 = 0xA5;
/// Size of the parameter identifier in a record.
const ID_SIZE: usize = 4;

/// A parameter of the dictionary.
#[derive(Debug, Clone)]
pub struct Param {
    pub component: String,
    pub name: String,
    pub id: u32,
    pub ty: Type,
    pub default: Option<serde_json::Value>,
}

impl Param {
    pub fn full_name(&self) -> String {
        format!("{}.{}", self.component, self.name)
    }
}

/// The parameters of a dictionary, by qualified name, and what is needed to serialize their values.
pub struct ParamDictionary {
    pub params: Vec<Param>,
    types: Dictionary,
}

impl ParamDictionary {
    pub fn load(path: &Path) -> std::result::Result<Self, String> {
        Self::from_json(&path.display().to_string(), fgds_offline::read_dictionary_json(path)?)
    }

    pub fn from_json(file: &str, mut json: serde_json::Value) -> std::result::Result<Self, String> {
        let list = json["parameters"].as_array().cloned().unwrap_or_default();
        // The dictionary loader resolves the types of commands: hand it each parameter as one.
        let commands: Vec<serde_json::Value> = list
            .iter()
            .enumerate()
            .map(|(i, p)| json!({"name": i.to_string(), "opcode": i, "formalParams": [{"name": "value", "type": p["type"]}]}))
            .collect();
        json["commands"] = commands.into();
        json["events"] = json!([]);
        json["telemetryChannels"] = json!([]);
        json["telemetryPacketSets"] = json!([]);
        let types = Dictionary::parse(file, &json.to_string())
            .map_err(|e| e.to_string().replace("commands[", "parameters["))?;
        let mut params = Vec::with_capacity(list.len());
        for (i, (item, command)) in list.iter().zip(&types.commands).enumerate() {
            let at = |problem: &str| format!("{file}: parameters[{i}]: {problem}");
            let full = item["name"].as_str().ok_or_else(|| at("missing string field \"name\""))?;
            let (component, name) = full
                .rsplit_once('.')
                .filter(|(c, n)| !c.is_empty() && !n.is_empty())
                .ok_or_else(|| at("the name is not component.parameter"))?;
            let id = item["id"].as_u64().and_then(|id| u32::try_from(id).ok());
            params.push(Param {
                component: component.to_string(),
                name: name.to_string(),
                id: id.ok_or_else(|| at("missing 32-bit field \"id\""))?,
                ty: command.params[0].ty.clone(),
                default: item.get("default").filter(|d| !d.is_null()).cloned(),
            });
        }
        // The order of a `.dat` file: by qualified name.
        params.sort_by_key(Param::full_name);
        Ok(ParamDictionary { params, types })
    }

    /// The parameters to write and their values: those of the JSON file, by qualified
    /// name, and with `defaults` also those that have a default in the dictionary.
    pub fn select<'a>(
        &'a self,
        file: &serde_json::Value,
        defaults: bool,
    ) -> std::result::Result<Vec<(&'a Param, serde_json::Value)>, String> {
        let components = file.as_object().ok_or("the parameter file is not a JSON object of components")?;
        for (component, values) in components {
            let values = values.as_object().ok_or_else(|| format!("{component} is not a JSON object of parameters"))?;
            for name in values.keys() {
                if !self.params.iter().any(|p| p.component == *component && p.name == *name) {
                    return Err(format!("Unable to find param {component}.{name} in dictionary"));
                }
            }
        }
        let mut selected = Vec::new();
        for param in &self.params {
            let given = components.get(&param.component).and_then(|c| c.get(&param.name));
            let value = given.or(param.default.as_ref().filter(|_| defaults));
            if let Some(value) = value.filter(|v| !v.is_null()) {
                selected.push((param, value.clone()));
            }
        }
        Ok(selected)
    }

    /// The bytes of a `.dat` file with these values.
    pub fn to_dat(&self, values: &[(&Param, serde_json::Value)]) -> std::result::Result<Vec<u8>, String> {
        let mut data = Vec::new();
        for (param, value) in values {
            let mut bytes = Vec::new();
            self.types
                .encode(&param.ty, value, &mut bytes)
                .map_err(|e| format!("{} [{}]: {}", param.full_name(), param.ty.name(), e.0))?;
            let size = u32::try_from(ID_SIZE + bytes.len()).map_err(|_| format!("{}: too long", param.full_name()))?;
            data.push(DELIMITER);
            data.extend_from_slice(&size.to_be_bytes());
            data.extend_from_slice(&param.id.to_be_bytes());
            data.extend_from_slice(&bytes);
        }
        let mut out = checksum(&data).to_be_bytes().to_vec();
        out.extend_from_slice(&data);
        Ok(out)
    }

    /// The values of a `.dat` file, in file order.
    pub fn from_dat<'a>(&'a self, dat: &[u8]) -> std::result::Result<Vec<(&'a Param, Value)>, String> {
        decode::from_dat(self, dat)
    }
}

/// The checksum at the start of a `.dat` file, over the bytes after it.
fn checksum(data: &[u8]) -> u32 {
    !fgds_offline::crc32(data)
}

/// The text of a command sequence that sets these values; with `save`, also saves each.
pub fn to_seq(values: &[(&Param, serde_json::Value)], save: bool) -> String {
    let mut lines = vec!["; Autocoded sequence file from JSON".to_string()];
    for (param, value) in values {
        let command = format!("{}.{}", param.component, param.name.to_uppercase());
        lines.push(format!("R00:00:00 {command}_PRM_SET {}", fgds_offline::json_line(value)));
        if save {
            lines.push(format!("{command}_PRM_SAVE"));
        }
    }
    lines.join("\n")
}

/// Decoded values as the JSON `prm-write` reads: component, parameter, value.
pub fn to_json(values: &[(&Param, Value)]) -> Value {
    let mut components = serde_json::Map::new();
    for (param, value) in values {
        let component = components.entry(param.component.clone()).or_insert_with(|| json!({}));
        component[&param.name] = value.clone();
    }
    Value::Object(components)
}

/// A text as it is, any other value as JSON.
fn shown(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_string)
}

/// Decoded values as text, grouped by component.
pub fn to_text(values: &[(&Param, Value)]) -> String {
    let mut lines = Vec::new();
    let mut current: Option<&str> = None;
    for (param, value) in values {
        if current != Some(&param.component) {
            if current.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("Component: {}", param.component));
            current = Some(&param.component);
        }
        let shown = match value {
            Value::String(text) => format!("\"{text}\""),
            other => other.to_string(),
        };
        lines.push(format!("  {} = {shown} (type: {}, id: {})", param.name, param.ty.name(), param.id));
    }
    lines.join("\n")
}

/// Decoded values as CSV: `Component,Parameter,Value,Type,ID`.
pub fn to_csv(values: &[(&Param, Value)]) -> String {
    let mut lines = vec!["Component,Parameter,Value,Type,ID".to_string()];
    for (param, value) in values {
        let mut shown = shown(value);
        if shown.contains([',', '"', '\n', '\r']) {
            shown = format!("\"{}\"", shown.replace('"', "\"\""));
        }
        lines.push(format!("{},{},{shown},{},{}", param.component, param.name, param.ty.name(), param.id));
    }
    lines.join("\n")
}

pub struct ParamsPlugin;

impl Plugin for ParamsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "params", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_subcommand(Arc::new(WriteCommand))?;
        reg.add_subcommand(Arc::new(DecodeCommand))
    }
}

/// `-d`: the short form `fprime-prm-write` and `fprime-prm-decode` have for `--dictionary`.
fn dictionary_arg() -> Arg {
    Arg::new("dictionary-short").short('d').value_name("DICTIONARY").help("Same as --dictionary")
}

/// The dictionary of `-d`, else the one of `--dictionary` or of the configuration.
fn dictionary_path(args: &ArgMatches, env: &CliEnv) -> Result<PathBuf> {
    match args.get_one::<String>("dictionary-short") {
        Some(path) => Ok(PathBuf::from(path)),
        None => fgds_offline::dictionary_path(env),
    }
}

fn output_arg() -> Arg {
    Arg::new("output")
        .short('o')
        .long("output")
        .help("Path of the output file [default: the input path with the extension of the format]")
}

/// Print the message of a failed tool and fail.
fn finish(result: std::result::Result<(), String>) -> Result<ExitCode> {
    Ok(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            fgds_core::err!("{message}");
            ExitCode::FAILURE
        }
    })
}

/// The checks before a tool starts; a refusal goes to the standard error.
fn begin(verb: &str, name: &str, input: &Path, dictionary: &Path, output: &Path, format: &str) -> Option<ExitCode> {
    let refused = if !input.exists() {
        format!("Unable to find {}", input.display())
    } else if input.is_dir() {
        format!("{name} is a dir {}", input.display())
    } else if !dictionary.exists() {
        format!("Unable to find {}", dictionary.display())
    } else {
        fgds_core::out!("{verb} {} to {} (format: .{format})", input.display(), output.display());
        return None;
    };
    fgds_core::err!("{refused}");
    Some(ExitCode::FAILURE)
}

/// `Done, writing to` and the resolved path, then the file.
fn write_file(path: &Path, bytes: &[u8]) -> std::result::Result<(), String> {
    let failed = |e: std::io::Error| format!("{}: {e}", path.display());
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(failed)?;
    let resolved = parent.canonicalize().map_err(failed)?.join(path.file_name().unwrap_or_default());
    fgds_core::out!("Done, writing to {}", resolved.display());
    std::fs::write(path, bytes).map_err(failed)
}

struct WriteCommand;

impl Subcommand for WriteCommand {
    fn command(&self) -> Command {
        let format = |name: &'static str, about: &'static str| {
            Command::new(name)
                .about(about)
                .arg(Arg::new("json_file").required(true).help("Path of the JSON file of parameter values"))
                .arg(
                    Arg::new("defaults")
                        .long("defaults")
                        .action(ArgAction::SetTrue)
                        .help("Also write the default value of each parameter the file does not give"),
                )
                .arg(output_arg())
                .arg(dictionary_arg())
        };
        let save = Arg::new("save")
            .long("save")
            .action(ArgAction::SetTrue)
            .help("Follow each parameter set command with its save command");
        Command::new("prm-write")
            .about("Write parameter values from JSON (as fprime-prm-write)")
            .subcommand_required(true)
            .subcommand(format("dat", "Write the parameter database file loaded by PrmDb"))
            .subcommand(format("seq", "Write a command sequence that sets the parameters").arg(save))
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let (format, args) = args.subcommand().expect("a format is required");
        let input = PathBuf::from(args.get_one::<String>("json_file").expect("required"));
        let output = args.get_one::<String>("output").map_or_else(|| input.with_extension(format), PathBuf::from);
        let dictionary = dictionary_path(args, env)?;
        if let Some(refused) = begin("Converting", "json-file", &input, &dictionary, &output, format) {
            return Ok(refused);
        }
        finish((|| {
            let bytes = fgds_offline::read_file(&input)?;
            let file = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", input.display()))?;
            let dict = ParamDictionary::load(&dictionary)?;
            let values = dict.select(&file, args.get_flag("defaults"))?;
            let out = match format {
                "dat" => dict.to_dat(&values)?,
                _ => to_seq(&values, args.get_flag("save")).into_bytes(),
            };
            write_file(&output, &out)
        })())
    }
}

struct DecodeCommand;

impl Subcommand for DecodeCommand {
    fn command(&self) -> Command {
        Command::new("prm-decode")
            .about("Decode a parameter database file (as fprime-prm-decode)")
            .arg(Arg::new("dat_file").required(true).help("Path of the parameter database file"))
            .arg(
                Arg::new("format")
                    .short('f')
                    .long("format")
                    .value_parser(["json", "text", "csv"])
                    .default_value("json")
                    .help("Output format"),
            )
            .arg(output_arg())
            .arg(dictionary_arg())
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let input = PathBuf::from(args.get_one::<String>("dat_file").expect("required"));
        let format = args.get_one::<String>("format").expect("has a default").as_str();
        let output = args.get_one::<String>("output").map_or_else(|| input.with_extension(format), PathBuf::from);
        let dictionary = dictionary_path(args, env)?;
        if let Some(refused) = begin("Decoding", "dat-file", &input, &dictionary, &output, format) {
            return Ok(refused);
        }
        finish((|| {
            let bytes = fgds_offline::read_file(&input)?;
            let dict = ParamDictionary::load(&dictionary)?;
            let values = dict.from_dat(&bytes)?;
            let text = match format {
                "json" => serde_json::to_string_pretty(&to_json(&values)).map_err(|e| e.to_string())?,
                "text" => to_text(&values),
                _ => to_csv(&values),
            };
            write_file(&output, text.as_bytes())
        })())
    }
}
