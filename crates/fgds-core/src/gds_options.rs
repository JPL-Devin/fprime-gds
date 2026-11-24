//! The GDS options every command of `fprime-cli` takes, for the subcommands of `fgds` that stand for one
//! (`command-send`, `events`, `channels`, `send-raw`, `file-uplink`, and the dictionary ones for `seqgen`):
//! one table for the argparse reading, the arguments clap then sees, and the one check made before
//! anything is done.

use crate::argparse::{Kind, Nargs, Opt};
use crate::clap::{Arg, ArgAction, ArgMatches};
use crate::serde_json::Value;
use crate::CliEnv;
use std::path::Path;
use std::process::ExitCode;

/// The GDS options every command of `fprime-cli` (fprime-gds 4.4.0) takes, in the order of its parser.
/// That order is the one `ambiguous option` lists its matches in. `fprime-cli` keeps its six groups of
/// GDS options (hash file, files, middleware, logs, history, dictionary) in a Python set, whose order
/// follows the addresses of the objects: the order here is the one it takes most often. Those that set
/// up the Python GDS itself are read and have no effect here.
pub fn options() -> Vec<Opt> {
    use Kind::{Float, Int, Str};
    use Nargs::{Exactly, Flag, One};
    const LEVELS: Kind = Kind::Choice(&["DEBUG", "INFO", "WARNING", "ERROR"]);
    vec![
        Opt::new(&["-h", "--help"], Flag, Str),
        Opt::new(&["--hash-file"], One, Str),
        Opt::new(&["--file-storage-directory"], One, Str),
        Opt::new(&["--file-uplink-cooldown"], One, Float).ignored(),
        Opt::new(&["--file-uplink-chunk-size"], One, Int).ignored(),
        Opt::new(&["--no-zmq"], Flag, Str).ignored(),
        Opt::new(&["--zmq-transport"], Exactly(2), Str).ignored(),
        Opt::new(&["--tts-port"], One, Int).ignored(),
        Opt::new(&["--tts-addr"], One, Str).ignored(),
        Opt::new(&["-l", "--logs"], One, Str),
        Opt::new(&["--log-directly"], Flag, Str),
        Opt::new(&["--log-to-stdout"], Flag, Str).ignored(),
        Opt::new(&["--log-level-gds"], One, LEVELS).ignored(),
        Opt::new(&["--disable-data-logging"], Flag, Str).ignored(),
        Opt::new(&["--log-prefix"], One, Str),
        Opt::new(&["--no-clear-history"], Flag, Str).ignored(),
        Opt::new(&["-d", "--deployment"], One, Str),
        Opt::new(&["--dictionary"], One, Str),
        Opt::new(&["--packet-spec"], One, Str),
        Opt::new(&["--packet-set-name"], One, Str),
    ]
}

/// The GDS options of `fprime-cli` that are checked here and are no option of `fgds` itself.
pub const NAMING: [&str; 6] =
    ["packet-spec", "packet-set-name", "deployment", "hash-file", "file-storage-directory", "log-prefix"];

pub fn args() -> Vec<Arg> {
    let mut args: Vec<Arg> = NAMING.iter().map(|name| Arg::new(*name).long(*name).hide(true)).collect();
    args.push(Arg::new("log-directly").long("log-directly").action(ArgAction::SetTrue).hide(true));
    args
}

pub const READ_AS_FPRIME_CLI: &str = "The command line is read as `fprime-cli` reads it: its options are accepted \
    with the same spelling, also shortened (`--arg`, `-i5`, `-l LOGS`). Those that name a dictionary, a file or a \
    directory are checked as `fprime-cli` checks them before anything is done; the others have no effect.";

/// Whether `fprime-cli` could make the directory `path`, and with `written` whether it could write in it.
/// Nothing is left here: a file is made and removed in the nearest directory that exists.
fn directory(path: &Path, written: bool) -> std::result::Result<(), String> {
    let path = if path.as_os_str().is_empty() { Path::new(".") } else { path };
    let nearest =
        path.ancestors().map(|a| if a.as_os_str().is_empty() { Path::new(".") } else { a }).find(|a| a.exists());
    let Some(nearest) = nearest else {
        return Err(format!("{} cannot be created", path.display()));
    };
    if !nearest.is_dir() {
        return Err(format!("{} is not a directory", nearest.display()));
    }
    if nearest == path && !written {
        return Ok(());
    }
    let probe = nearest.join(format!(".fgds-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => std::fs::remove_file(&probe).map_err(|e| e.to_string()),
        Err(error) if nearest == path => Err(format!("{} is not writable: {error}", path.display())),
        Err(error) => Err(format!("{} cannot be created in {}: {error}", path.display(), nearest.display())),
    }
}

pub fn has_packet_set(dictionary: &Value, name: &str) -> std::result::Result<(), (u8, String)> {
    let sets = dictionary["telemetryPacketSets"].as_array().cloned().unwrap_or_default();
    if sets.iter().any(|set| set["name"] == name) {
        return Ok(());
    }
    Err((1, format!("the dictionary does not contain packet set '{name}'")))
}

/// The dictionary `fprime-cli` finds in a deployment: the one `*Dictionary.json` of its `dict` directory.
pub fn deployment_dictionary(deployment: &str) -> std::result::Result<std::path::PathBuf, (u8, String)> {
    let dict = Path::new(deployment).join("dict");
    let entries = std::fs::read_dir(&dict)
        .map_err(|_| (255, format!("dictionary location {} does not exist", dict.display())))?;
    let mut found: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().ends_with("Dictionary.json")))
        .collect();
    match found.len() {
        0 => Err((255, format!("No dictionary found in dictionary location {}", dict.display()))),
        1 => Ok(found.remove(0)),
        _ => Err((255, format!("Multiple dictionaries of same type found in dictionary location {}", dict.display()))),
    }
}

/// The one check of the GDS options of `fprime-cli`, made by `command-send`, `events` and `channels` before
/// anything else, a listing included: what the options name is there, as `fprime-cli` requires. The checks
/// are those of `StandardPipelineParser` in the order it most often makes them (its parts are in a Python
/// set): the dictionary (`--dictionary`, else the one of `--deployment`; the packet specification, the
/// packet set), the hash file, the directory of files, the directory of logs. The error is the exit code
/// of `fprime-cli` and what is missing. The result is the dictionary the options name, when they name one:
/// the names are looked up and listed from it. `listing`: `fprime-cli` writes no file for a listing.
pub fn hold(args: &ArgMatches, env: &CliEnv, listing: bool) -> std::result::Result<Option<Value>, (u8, String)> {
    let given = |name: &str| args.try_get_one::<String>(name).ok().flatten();
    let read = |path: &Path| {
        std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| crate::serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
            .map_err(|e| (1, format!("dictionary file {}: {e}", path.display())))
    };
    let dictionary = if let Some(path) = given("dictionary") {
        if !Path::new(path).is_file() {
            return Err((1, format!("dictionary file {path} does not exist")));
        }
        (env.dictionary)().map_err(|e| (1, format!("dictionary file {path}: {e}")))?;
        Some(read(Path::new(path))?)
    } else if let Some(deployment) = given("deployment").filter(|d| !d.is_empty()) {
        // `fprime-cli` looks for the dictionary in the deployment when none is given.
        Some(read(&deployment_dictionary(deployment)?)?)
    } else {
        None
    };
    if let Some(path) = given("packet-spec") {
        if !Path::new(path).is_file() {
            return Err((1, format!("packet specification {path} does not exist")));
        }
        // `fprime-cli` reads it as XML; here only its beginning is looked at.
        let text = std::fs::read(path).map_err(|e| (1, format!("packet specification {path}: {e}")))?;
        let text = String::from_utf8_lossy(&text);
        if !text.trim_start_matches('\u{feff}').trim_start().starts_with('<') {
            return Err((1, format!("packet specification {path} is not XML")));
        }
    }
    if let (Some(name), Some(dictionary)) = (given("packet-set-name"), &dictionary) {
        has_packet_set(dictionary, name)?;
    }
    // `fprime-cli` takes an empty text as no hash file; here it names a file that is not there.
    if let Some(path) = given("hash-file").filter(|path| !Path::new(path).exists()) {
        return Err((255, format!("hash file location {path} does not exist")));
    }
    if let Some(path) = given("file-storage-directory") {
        directory(Path::new(path), !listing).map_err(|e| (1, format!("file storage directory: {e}")))?;
    }
    // `fprime-cli` writes its log in `<logs>/<prefix>-<time>`, with `--log-directly` in `<logs>`.
    let direct = args.try_get_one::<bool>("log-directly").ok().flatten().copied().unwrap_or(false);
    if given("logs").is_some() || given("log-prefix").is_some() {
        let logs = given("logs").map(String::as_str).unwrap_or("logs");
        if direct && logs.is_empty() {
            return Err((1, "log directory: no directory is named".to_string()));
        }
        let prefix = given("log-prefix").map(String::as_str).unwrap_or("fprime-cli");
        let dated = Path::new(logs).join(format!("{prefix}-"));
        let into = if direct { Path::new(logs) } else { dated.parent().unwrap_or(Path::new(logs)) };
        directory(into, true).map_err(|e| (1, format!("log directory: {e}")))?;
    }
    Ok(dictionary)
}

/// A command line `fprime-cli` ends before doing anything: what is missing and its exit code.
pub fn not_done(what: &str, (code, missing): (u8, String)) -> ExitCode {
    crate::err!("nothing was {what}: {missing}");
    ExitCode::from(code)
}

/// The dictionary options alone (`fprime-seqgen` takes these of the GDS options): the same rows of the table.
pub fn dictionary_options() -> Vec<Opt> {
    const KEPT: [&str; 5] = ["--help", "--deployment", "--dictionary", "--packet-spec", "--packet-set-name"];
    options().into_iter().filter(|o| o.strings.iter().any(|s| KEPT.contains(&s.as_str()))).collect()
}
