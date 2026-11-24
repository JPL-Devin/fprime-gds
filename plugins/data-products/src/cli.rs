//! `fgds dp decode` and `fgds dp validate`: the command line of `fprime-dp`, with the
//! same output, messages and exit codes.

use crate::decode::{decode, DecodeOptions, DpError, DEFAULT_MAX_BYTES};
use crate::types::DpDictionary;
use crate::validate::{validate, HeaderSize, Invalid, MIN_PRODUCT};
use crate::{io_error, OPTION_MAX_BYTES, PLUGIN};
use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::{CliEnv, Result, Subcommand};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub struct DpCommand;

const DECODE_USAGE: &str = "decode [-h] (-b BIN_FILE | -B BIN_DIR) -d DICTIONARY [-o OUTPUT] [-r] [-z]";
const VALIDATE_USAGE: &str = "validate [-h] -b BIN_FILE [-d DICTIONARY] [-s HEADER_SIZE] [-g] [-v]";

impl Subcommand for DpCommand {
    fn command(&self) -> Command {
        let value = |name: &'static str, short, help: &'static str| Arg::new(name).short(short).long(name).help(help);
        let flag = |name, short, help| value(name, short, help).action(ArgAction::SetTrue);
        // Required and exclusive arguments are checked in `run`, to answer as `fprime-dp` does.
        let decode = Command::new("decode")
            .about("Decode a data product binary into a human-readable format")
            .args_override_self(true)
            .arg(value("bin-file", 'b', "Path to input data product binary file (.fdp)"))
            .arg(value(
                "bin-dir",
                'B',
                "Directory of data product binary files; every *.fdp file in it is decoded to <binFilename>.json",
            ))
            .arg(value("dictionary", 'd', "Path to F Prime JSON Dictionary"))
            .arg(value(
                "output",
                'o',
                "Path to output JSON file (defaults to <binFilename>.json). Not allowed with --bin-dir",
            ))
            .arg(flag(
                "rename-id",
                'r',
                "Name output files <containerName>_<sec>_<usec>.json instead of Dp_<containerId>_<sec>_<usec>.json",
            ))
            .arg(flag("disable-decompression", 'z', "Disable automatic decompression of data products"));
        let validate = Command::new("validate")
            .about("Validate a data product")
            .args_override_self(true)
            .arg(value("bin-file", 'b', "Path to input data product binary file (.fdp)"))
            .arg(value("dictionary", 'd', "Path to F Prime JSON Dictionary"))
            .arg(
                value("header-size", 's', "Use the provided value as the header size for the data product")
                    .allow_negative_numbers(true),
            )
            .arg(flag("guess-size", 'g', "Guess at the header size"))
            .arg(flag("verbose", 'v', "Verbose output"));
        Command::new("dp")
            .about("Decode and validate data product files, without a running GDS")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand(decode)
            .subcommand(validate)
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "data-products-max-bytes"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let max_bytes = env.config.plugin(PLUGIN).get(OPTION_MAX_BYTES).and_then(|v| v.parse().ok());
        let outcome = match args.subcommand() {
            Some(("decode", args)) => run_decode(args, max_bytes.unwrap_or(DEFAULT_MAX_BYTES)),
            Some(("validate", args)) => {
                let given = args.value_source("data-products-max-bytes");
                if given == Some(fgds_core::clap::parser::ValueSource::CommandLine) {
                    fgds_core::err!("fgds: error: --data-products-max-bytes is an option of `fgds dp decode`; `fgds dp validate` cannot honour it");
                    return Ok(ExitCode::from(2));
                }
                run_validate(args)
            }
            _ => unreachable!("clap requires a subcommand"),
        };
        Ok(match outcome {
            Ok(code) => code,
            Err(Stop::Usage(usage, message)) => {
                let program = std::env::args().next().unwrap_or_default();
                let program =
                    Path::new(&program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let command = usage.split(' ').next().unwrap_or_default();
                fgds_core::err!("usage: {program} dp {usage}");
                fgds_core::err!("{program} dp {command}: error: {message}");
                ExitCode::from(2)
            }
            Err(Stop::Failure(message)) => {
                fgds_core::err!("{message}");
                ExitCode::FAILURE
            }
        })
    }
}

enum Stop {
    /// A mistake in the arguments: usage, message, exit code 2.
    Usage(&'static str, String),
    /// Exit code 1 after this text on the error output.
    Failure(String),
}

fn text<'a>(args: &'a ArgMatches, name: &str) -> Option<&'a str> {
    args.get_one::<String>(name).map(String::as_str)
}

fn required(usage: &'static str, name: &str) -> Stop {
    Stop::Usage(usage, format!("the following arguments are required: {name}"))
}

/// A path as `str(pathlib.Path(text))` writes it.
fn python_path(text: &str) -> String {
    let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
    match (text.starts_with('/'), parts.is_empty()) {
        (true, _) => format!("/{}", parts.join("/")),
        (false, true) => ".".into(),
        (false, false) => parts.join("/"),
    }
}

/// The absolute path of an output file, to detect two products written to one file.
fn resolved(path: &str) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    match (path.parent().and_then(|parent| parent.canonicalize().ok()), path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path,
    }
}

/// The start and end of the first `Dp_<digits>_` in `stem`, and the digits.
fn container_id_in(stem: &str) -> Option<(usize, usize, &str)> {
    stem.match_indices("Dp_").find_map(|(start, _)| {
        let digits_start = start + 3;
        let digits = stem[digits_start..].bytes().take_while(u8::is_ascii_digit).count();
        let end = digits_start + digits;
        (digits > 0 && stem[end..].starts_with('_')).then(|| (start, end + 1, &stem[digits_start..end]))
    })
}

/// The output path of `--rename-id`: `Dp_<container id>` replaced by the container's short name.
fn renamed_output(dict: &DpDictionary, bin_file: &str) -> Option<String> {
    let path = Path::new(bin_file);
    let name = path.file_name()?.to_str()?;
    let stem = path.file_stem()?.to_str()?;
    let Some((start, end, digits)) = container_id_in(stem) else {
        fgds_core::err!("Warning: {name} does not match Dp_<id>_<sec>_<usec> naming, keeping file name");
        return None;
    };
    let id = digits.trim_start_matches('0');
    let Some(container) = dict.containers.get(&id.parse().unwrap_or(if id.is_empty() { 0 } else { -1 })) else {
        fgds_core::err!(
            "Warning: container id {} ({name}) not found in dictionary, keeping file name",
            if id.is_empty() { "0" } else { id }
        );
        return None;
    };
    let short_name = container.name.rsplit('.').next().unwrap_or(&container.name);
    let renamed = format!("{}{short_name}_{}.json", &stem[..start], &stem[end..]);
    Some(python_path(&path.with_file_name(renamed).to_string_lossy()))
}

fn default_output(bin_file: &str) -> String {
    python_path(&Path::new(bin_file).with_extension("json").to_string_lossy())
}

/// Decode one file and write its JSON. `Ok(false)` is a product that does not decode.
fn decode_file(
    dict: &DpDictionary,
    bin_file: &str,
    output: &str,
    options: DecodeOptions,
) -> std::result::Result<bool, String> {
    fgds_core::out!("Decoding {bin_file}...");
    let run = || {
        let size = std::fs::metadata(bin_file).map_err(|e| DpError::Unexpected(io_error(&e, bin_file)))?.len();
        if size > options.max_bytes {
            return Err(crate::decode::over_limit(options.max_bytes));
        }
        let data = std::fs::read(bin_file).map_err(|e| DpError::Unexpected(io_error(&e, bin_file)))?;
        let decoded = decode(dict, &data, options)?;
        std::fs::write(output, decoded.to_json().to_python_text())
            .map_err(|e| DpError::Unexpected(io_error(&e, output)))
    };
    match run() {
        Ok(()) => {
            fgds_core::out!("Decoding complete: {output}");
            Ok(true)
        }
        Err(DpError::Product(message)) => {
            fgds_core::err!("Error: {message}");
            Ok(false)
        }
        Err(DpError::Unexpected(message)) => {
            fgds_core::err!("Unexpected error: {message}");
            Err(message)
        }
    }
}

fn run_decode(args: &ArgMatches, max_bytes: u64) -> std::result::Result<ExitCode, Stop> {
    let usage = |message: String| Stop::Usage(DECODE_USAGE, message);
    let (bin_file, bin_dir) = (text(args, "bin-file"), text(args, "bin-dir"));
    if bin_file.is_some() && bin_dir.is_some() {
        // argparse names the argument it met second.
        let file_last = args.index_of("bin-file") > args.index_of("bin-dir");
        let (later, earlier) =
            if file_last { ("-b/--bin-file", "-B/--bin-dir") } else { ("-B/--bin-dir", "-b/--bin-file") };
        return Err(usage(format!("argument {later}: not allowed with argument {earlier}")));
    }
    let dictionary = text(args, "dictionary").ok_or_else(|| required(DECODE_USAGE, "-d/--dictionary"))?;
    if bin_file.is_none() && bin_dir.is_none() {
        return Err(usage("one of the arguments -b/--bin-file -B/--bin-dir is required".into()));
    }
    let dict = DpDictionary::load(dictionary).map_err(|e| Stop::Failure(format!("error: {e}")))?;
    let output = text(args, "output");
    let bin_files = match (bin_file, bin_dir) {
        (Some(file), _) => vec![python_path(file)],
        (None, Some(dir)) => {
            if output.is_some() {
                return Err(usage("--output cannot be used with --bin-dir".into()));
            }
            let entries = std::fs::read_dir(dir).map_err(|_| usage(format!("{dir} is not a directory")))?;
            let mut names: Vec<String> = entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()))
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.ends_with(".fdp"))
                .collect();
            names.sort();
            let dir = python_path(dir);
            if names.is_empty() {
                return Err(Stop::Failure(format!("No .fdp files found in {dir}")));
            }
            names.iter().map(|name| python_path(&format!("{dir}/{name}"))).collect()
        }
        (None, None) => unreachable!("checked above"),
    };
    let options = DecodeOptions { decompress: !args.get_flag("disable-decompression"), max_bytes };
    let mut failures = 0;
    let mut written: HashMap<PathBuf, &str> = HashMap::new();
    for bin_file in &bin_files {
        let renamed =
            if output.is_none() && args.get_flag("rename-id") { renamed_output(&dict, bin_file) } else { None };
        let output = output.map(str::to_string).or(renamed).unwrap_or_else(|| default_output(bin_file));
        let resolved = resolved(&output);
        if let Some(earlier) = written.get(&resolved) {
            fgds_core::err!("Error: skipping {bin_file}: output {} already written from {earlier}", resolved.display());
            failures += 1;
            continue;
        }
        match decode_file(&dict, bin_file, &output, options) {
            Ok(true) => drop(written.insert(resolved, bin_file)),
            Ok(false) => failures += 1,
            Err(message) => {
                fgds_core::err!("Error: failed to decode {bin_file}: {message}");
                failures += 1;
            }
        }
    }
    if bin_dir.is_some() {
        fgds_core::out!("Decoded {}/{} data products", bin_files.len() - failures, bin_files.len());
    }
    Ok(if failures > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn run_validate(args: &ArgMatches) -> std::result::Result<ExitCode, Stop> {
    let header_size = match text(args, "header-size") {
        Some(given) => given.trim().parse::<i64>().map_err(|_| {
            Stop::Usage(VALIDATE_USAGE, format!("argument -s/--header-size: invalid int value: '{given}'"))
        })?,
        None => 0,
    };
    let bin_file = text(args, "bin-file").ok_or_else(|| required(VALIDATE_USAGE, "-b/--bin-file"))?;
    let dict = match text(args, "dictionary") {
        Some(path) => Some(DpDictionary::load(path).map_err(|e| Stop::Failure(format!("error: {e}")))?),
        None => None,
    };
    let crash = |reason: String| Stop::Failure(format!("Unable to validate Data Product file {bin_file}\n{reason}"));
    let data = std::fs::read(bin_file).map_err(|e| crash(io_error(&e, bin_file)))?;
    let header_size = match (header_size, &dict) {
        (size, _) if size > 0 => HeaderSize::Given(size as usize),
        // A file too small for any header is reported as such before the dictionary is used.
        (_, Some(dict)) if data.len() >= MIN_PRODUCT => {
            let header =
                dict.header_type().and_then(|ty| dict.max_size(&crate::types::Ty::Struct(ty))).map_err(crash)?;
            HeaderSize::Dictionary(header as usize)
        }
        _ => HeaderSize::Guess,
    };
    let validation = validate(&data, header_size);
    if args.get_flag("verbose") {
        validation.verbose.iter().for_each(|line| fgds_core::out!("{line}"));
    }
    match validation.result {
        Ok(()) => {
            fgds_core::out!("Validation OK!");
            Ok(ExitCode::SUCCESS)
        }
        Err(Invalid::Failed(message)) => Err(Stop::Failure(message)),
        Err(Invalid::Crash(reason)) => Err(crash(reason)),
    }
}
