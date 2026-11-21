//! Compiler for classic `Svc::CmdSequencer` sequence files: the function of `fprime-seqgen`
//! (SEQ-1, CLI-5), as a library and as the `fgds seqgen` subcommand.
//!
//! The result is byte-identical to the Python tool for every file it accepts, and an error
//! naming the file and line for every file it rejects. The grammar is the one of the Python
//! GDS: the parser runs the tables Lark builds from it (`tools/gen_tables.py`).

pub mod convert;
pub mod lexer;
pub mod parser;
mod pyjson;
mod tables;
pub mod time;
pub mod value;

use convert::{convert, type_name, Source};
use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::dictionary::Type;
use fgds_core::serde_json::{self, json};
use fgds_core::{CliEnv, Dictionary, Error, Manifest, Plugin, Registry, Result, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// Time base written when none is given: any (`TB_DONT_CARE`).
pub const TIMEBASE_ANY: u32 = 0xFFFF;

/// Largest sequence or dictionary file read (LNK-9).
pub const MAX_INPUT: u64 = 256 * 1024 * 1024;

const CRC32: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// The checksum of sequence and parameter files (`zlib.crc32`).
pub fn crc32(bytes: &[u8]) -> u32 {
    CRC32.checksum(bytes)
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

/// The Python loader reads a type reference by its name alone; give the references that have
/// only a name the fields the dictionary loader reads.
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

fn put(dict: &Dictionary, name: &str, default: Type, value: u64, out: &mut Vec<u8>) -> std::result::Result<(), String> {
    let ty = match dict.config_type(name, default.clone()) {
        Type::Enum { rep, .. } => *rep,
        ty @ Type::Int { .. } => ty,
        _ => default,
    };
    dict.encode(&ty, &json!(value), out).map_err(|e| format!("{name}: {}", e.0))
}

/// One record of the binary file: descriptor, time, length and the command packet.
fn record(
    dict: &Dictionary,
    statement: &parser::Statement,
    time: time::Time,
    out: &mut Vec<u8>,
) -> std::result::Result<(), String> {
    let Some(command) = dict.command(&statement.mnemonic) else {
        return Err(format!("'{}' does not match any command in the command dictionary.", statement.mnemonic));
    };
    let mut packet = Vec::new();
    put(dict, "FwPacketDescriptorType", Type::U16, 0, &mut packet)?;
    put(dict, "FwOpcodeType", Type::U32, u64::from(command.opcode), &mut packet)?;
    // As the Python tool: arguments beyond the command's are ignored, missing ones are not written.
    let mut errors = Vec::new();
    for (param, value) in command.params.iter().zip(&statement.args) {
        errors.push(match convert(dict, &param.ty, value, Source::Argument, &mut packet) {
            Ok(()) => String::new(),
            Err(e) => format!("{}[{}]: {e}", param.name, type_name(&param.ty)),
        });
    }
    if errors.iter().any(|e| !e.is_empty()) {
        return Err(format!("{} errored: {}", statement.mnemonic, errors.join(",")));
    }
    let length = u32::try_from(packet.len()).map_err(|_| format!("{}: command too long", statement.mnemonic))?;
    out.push(u8::from(statement.relative));
    out.extend_from_slice(&time.seconds.to_be_bytes());
    out.extend_from_slice(&time.useconds.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&packet);
    Ok(())
}

/// Compile the text of a sequence file to the binary `Svc::CmdSequencer` loads.
///
/// `file` is the name of the sequence in error messages (the Python tool writes the absolute
/// path). With `continue_on_error` every error of the file is reported, one per line,
/// instead of the first.
pub fn compile(
    text: &str,
    file: &str,
    dict: &Dictionary,
    timebase: u32,
    continue_on_error: bool,
) -> std::result::Result<Vec<u8>, String> {
    let statements = parser::parse(text).map_err(|e| {
        let token = match &e.token {
            Some((kind, text)) => format!(": unexpected token 'Token('{kind}', {})'", value::str_repr(text)),
            None => String::new(),
        };
        format!("Parsing error: {file}:{}:{}: Encountered syntax error parsing{token}", e.line, e.column)
    })?;

    let mut records = Vec::new();
    let mut count: u32 = 0;
    // Errors of the text of a statement, then errors against the dictionary.
    let (mut parsing, mut commands) = (Vec::new(), Vec::new());
    for statement in &statements {
        let line = statement.line;
        let time = match &statement.arg_error {
            Some(e) => Err(e.clone()),
            None => time::parse(&statement.time, !statement.relative),
        };
        let time = match time {
            Ok(time) => time,
            Err(e) if continue_on_error => {
                parsing.push(format!("{file}:{line}: {e}"));
                continue;
            }
            Err(e) => {
                return Err(format!("Parsing error: {file}:{line}: Encountered syntax error parsing timestamp: {e}"))
            }
        };
        match record(dict, statement, time, &mut records) {
            Ok(()) => count = count.saturating_add(1),
            Err(e) if continue_on_error => commands.push(format!("{file}:{line}: {e}")),
            Err(e) => return Err(format!("{file}:{line}: {e}")),
        }
    }
    if !parsing.is_empty() {
        commands.insert(0, format!("Parsing error: {}", parsing.join("\n")));
    }
    if !commands.is_empty() {
        return Err(commands.join("\n"));
    }

    let size = u32::try_from(records.len() + 4).map_err(|_| format!("{file}: the sequence is larger than 4 GiB"))?;
    let mut out = Vec::with_capacity(records.len() + 15);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    put(dict, "FwTimeBaseStoreType", Type::U16, u64::from(timebase), &mut out)
        .map_err(|_| format!("time base {timebase:#x} does not fit the time base field"))?;
    put(dict, "FwTimeContextStoreType", Type::U8, 0xFF, &mut out)?;
    out.extend_from_slice(&records);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(out)
}

/// Compile the sequence file `input` with the dictionary file `dictionary`.
pub fn compile_file(
    input: &Path,
    dictionary: &Path,
    timebase: u32,
    continue_on_error: bool,
) -> std::result::Result<Vec<u8>, String> {
    let bytes = read_file(input)?;
    let dict = load_dictionary(dictionary)?;
    let file = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf()).display().to_string();
    let text = String::from_utf8(bytes).map_err(|e| {
        let line = e.as_bytes()[..e.utf8_error().valid_up_to()].iter().filter(|b| **b == b'\n').count() + 1;
        format!("Parsing error: {file}:{line}: the file is not UTF-8 text")
    })?;
    compile(&text, &file, &dict, timebase, continue_on_error)
}

/// The dictionary named on the command line or in the configuration.
pub fn dictionary_path(env: &CliEnv) -> Result<PathBuf> {
    if env.config.dictionary == Path::new("") {
        return Err(Error::Config("no dictionary: give --dictionary or set `dictionary` in the configuration".into()));
    }
    Ok(env.config.dictionary.clone())
}

pub struct SequencesPlugin;

impl Plugin for SequencesPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "sequences", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_subcommand(Arc::new(SeqgenCommand))
    }
}

struct SeqgenCommand;

impl Subcommand for SeqgenCommand {
    fn command(&self) -> Command {
        Command::new("seqgen")
            .about("Build the binary file of a command sequence (as fprime-seqgen)")
            .arg(Arg::new("sequence").required(true).help("Path of the sequence text file"))
            .arg(Arg::new("output").help("Path of the binary file [default: the sequence path with .bin]"))
            .arg(Arg::new("timebase").short('t').long("timebase").help("Time base of the sequence [default: any]"))
            .arg(
                Arg::new("all-errors")
                    .long("all-errors")
                    .action(ArgAction::SetTrue)
                    .help("Report every error of the sequence, not only the first"),
            )
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let input = PathBuf::from(args.get_one::<String>("sequence").expect("required"));
        let timebase = match args.get_one::<String>("timebase") {
            None => TIMEBASE_ANY,
            Some(text) => match value::parse_int(text).and_then(|i| i.to_i128()).and_then(|i| u32::try_from(i).ok()) {
                Some(timebase) => timebase,
                None => {
                    eprintln!("Could not parse time base {text}");
                    return Ok(ExitCode::FAILURE);
                }
            },
        };
        let output = match args.get_one::<String>("output") {
            Some(output) => PathBuf::from(output),
            None => input.with_extension("bin"),
        };
        let result = compile_file(&input, &dictionary_path(env)?, timebase, args.get_flag("all-errors"));
        let binary = match result {
            Ok(binary) => binary,
            Err(message) => {
                eprintln!("{message}");
                return Ok(ExitCode::FAILURE);
            }
        };
        if let Err(e) = std::fs::write(&output, &binary) {
            eprintln!("Encountered problem opening output file '{}': {e}", output.display());
            return Ok(ExitCode::FAILURE);
        }
        let shown = if timebase == TIMEBASE_ANY { "ANY".to_string() } else { format!("{timebase:#x}") };
        println!("{}: {} bytes with timebase {shown}", output.display(), binary.len());
        Ok(ExitCode::SUCCESS)
    }
}
