//! Compiler for classic `Svc::CmdSequencer` sequence files: the function of `fprime-seqgen`
//! (SEQ-1, CLI-5), as a library and as the `fgds seqgen` subcommand.
//!
//! The result is the binary `fprime-seqgen` writes for a valid file (`fixtures/sequences`), and an
//! error naming the file and line for every file that is refused.

pub mod parser;
pub mod time;

pub use fgds_offline::{
    crc32, dictionary_path, json_line, load_dictionary, read_dictionary_json, read_file, MAX_INPUT,
};

use fgds_core::axum::extract::State;
use fgds_core::axum::http::StatusCode;
use fgds_core::axum::routing::post;
use fgds_core::axum::{Json, Router};
use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::dictionary::Type;
use fgds_core::serde_json::json;
use fgds_core::serde_json::Value;
use fgds_core::{
    Access, ApiError, CliEnv, Context, Dictionary, Manifest, Plugin, Registry, Result, RouteAccess, Routes,
    StaticFiles, Subcommand, UiBundle,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// Time base written when none is given: any (`TB_DONT_CARE`).
pub const TIMEBASE_ANY: u32 = 0xFFFF;

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
    // The Python tool ignores arguments beyond the command's and writes a packet without the missing ones;
    // here the count is checked as for a command that is sent (`Dictionary::encode_command`).
    if statement.args.len() != command.params.len() {
        let (takes, given) = (command.params.len(), statement.args.len());
        return Err(format!("{} takes {takes} arguments, {given} given", statement.mnemonic));
    }
    let mut errors = Vec::new();
    for (param, value) in command.params.iter().zip(&statement.args) {
        // An array or object goes on as JSON text: read against the type of an array or struct
        // argument, the text itself for any other.
        let value = match (value, &param.ty) {
            (Value::Array(_) | Value::Object(_), Type::Array { .. } | Type::Struct { .. }) => json!(value.to_string()),
            (Value::Array(_) | Value::Object(_), _) => json!(json_line(value)),
            _ => value.clone(),
        };
        errors.push(match dict.encode(&param.ty, &value, &mut packet) {
            Ok(()) => String::new(),
            Err(e) => format!("{}: {}", param.name, e.0),
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
        let found = match e.found {
            Some('\n') => ": unexpected end of line".to_string(),
            Some(c) => format!(": unexpected {c:?}"),
            None => ": unexpected end of file".to_string(),
        };
        format!("{file}:{}:{}: syntax error{found}", e.line, e.column)
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

pub struct SequencesPlugin;

impl Plugin for SequencesPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "sequences", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_subcommand(Arc::new(SeqgenCommand))?;
        reg.add_tab(UiBundle {
            tab_id: "sequences",
            title: Some("Sequences"),
            order: 90,
            module: "sequences.js",
            files: &UI_FILES,
        })?;
        reg.add_routes(Routes {
            prefix: "sequences",
            router: Router::new().route("/compile", post(compile_route)),
            // Building a binary changes nothing in the GDS.
            access: vec![RouteAccess { method: "POST", path: "/compile", access: Access::Read }],
        })
    }
}

/// The tab module, embedded in the binary.
static UI_FILES: StaticFiles = StaticFiles(&[("sequences.js", include_bytes!("../ui/sequences.js"))]);

/// `{"name", "text"}` gives the binary of `fgds seqgen` for that text, as hexadecimal, with what the
/// tool prints (`output`); a sequence with an error is answered 422 with the message of the tool.
/// `name` is the path the messages name.
async fn compile_route(State(ctx): State<Context>, Json(request): Json<Value>) -> Result<Json<Value>, ApiError> {
    let text = request["text"].as_str().ok_or_else(|| ApiError::bad_request("text: a string is required"))?;
    let name = request["name"].as_str().unwrap_or("sequence.seq");
    let binary = compile(text, name, &ctx.dictionary, TIMEBASE_ANY, true)
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", e))?;
    let hex: String = binary.iter().map(|b| format!("{b:02x}")).collect();
    Ok(Json(json!({"hex": hex, "size": binary.len(), "output": summary(&binary, TIMEBASE_ANY)})))
}

/// What `fprime-seqgen` prints for a sequence it built: the size without the 11-byte header and the
/// checksum, which ends the file.
fn summary(binary: &[u8], timebase: u32) -> String {
    let shown = if timebase == TIMEBASE_ANY { "ANY".to_string() } else { format!("{timebase:#x}") };
    let (records, crc) = (binary.len() - 15, &binary[binary.len() - 4..]);
    let crc = u32::from_be_bytes(crc.try_into().expect("four bytes"));
    format!("Sequence is {records} bytes with timebase {shown}\nCRC: {crc} (0x{crc:04X})\n")
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

    /// Messages go to the standard output, as with `fprime-seqgen`.
    fn honours(&self) -> &'static [&'static str] {
        &["dictionary"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let input = PathBuf::from(args.get_one::<String>("sequence").expect("required"));
        let timebase = match args.get_one::<String>("timebase") {
            None => TIMEBASE_ANY,
            Some(text) => match fgds_core::dictionary::input::integer(text).and_then(|i| u32::try_from(i).ok()) {
                Some(timebase) => timebase,
                None => {
                    fgds_core::out!("Could not parse time base {text}");
                    return Ok(ExitCode::FAILURE);
                }
            },
        };
        let output = match args.get_one::<String>("output") {
            Some(output) => PathBuf::from(output),
            None => input.with_extension("bin"),
        };
        let dictionary = dictionary_path(env)?;
        let result = compile_file(&input, &dictionary, timebase, args.get_flag("all-errors"));
        let binary = match result {
            Ok(binary) => binary,
            Err(message) => {
                // One line per error: each begins with the path of the file. A line end elsewhere is of the
                // file name or of the sequence, and is written out.
                let file = std::path::absolute(&input).unwrap_or_else(|_| input.clone()).display().to_string();
                for (number, error) in message.split(&format!("\n{file}:")).enumerate() {
                    match number {
                        0 => fgds_core::out!("{error}"),
                        _ => fgds_core::out!("{file}:{error}"),
                    }
                }
                return Ok(ExitCode::FAILURE);
            }
        };
        if let Err(e) = std::fs::write(&output, &binary) {
            fgds_core::out!("Encountered problem opening output file '{}': {e}", output.display());
            return Ok(ExitCode::FAILURE);
        }
        print!("{}", summary(&binary, timebase));
        Ok(ExitCode::SUCCESS)
    }
}
