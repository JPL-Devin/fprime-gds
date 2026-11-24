//! The command line against a running GDS (CLI-3): `events`, `channels`, `commands`
//! and `command-send`, the functions of `fprime-cli`. Everything goes through the
//! public HTTP and WebSocket API (CLI-2); nothing here reads the GDS's own state.

mod api;
mod items;

use api::Api;
use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::serde_json::{json, Value};
use fgds_core::{CliEnv, Error, Manifest, Plugin, Registry, Result, Subcommand};
use items::{Filter, Item};
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

pub struct OnlinePlugin;

impl Plugin for OnlinePlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "online", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_subcommand(Arc::new(Records { name: "events", what: "events", kind: "fprime.event" }))?;
        reg.add_subcommand(Arc::new(Records { name: "channels", what: "channel samples", kind: "fprime.channel" }))?;
        reg.add_subcommand(Arc::new(Commands))?;
        reg.add_subcommand(Arc::new(CommandSend))?;
        reg.add_subcommand(Arc::new(SendRaw))
    }
}

fn url_arg() -> Arg {
    Arg::new("url").long("url").value_name("URL").help("Address of the running GDS [default: the configured --bind]")
}

/// What was asked for is not done: why, on standard error.
fn not_done(what: &str, missing: impl std::fmt::Display) -> ExitCode {
    fgds_core::err!("nothing was {what}: {missing}");
    ExitCode::FAILURE
}

fn filter_args() -> [Arg; 4] {
    [
        Arg::new("ids")
            .short('i')
            .long("ids")
            .value_name("ID")
            .num_args(1..)
            .action(ArgAction::Append)
            .value_parser(fgds_core::clap::value_parser!(u32))
            .help("Only items with one of these identifiers"),
        Arg::new("components")
            .short('c')
            .long("components")
            .value_name("COMP")
            .num_args(1..)
            .action(ArgAction::Append)
            .help("Only items of one of these components (the name without its last part)"),
        Arg::new("search")
            .short('s')
            .long("search")
            .value_name("STRING")
            .help("Only items whose line contains this; the line is searched as decoded, before its control characters are written out"),
        Arg::new("json").short('j').long("json").action(ArgAction::SetTrue).help("Print one JSON object per item"),
    ]
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(Error::Io)
}

fn api(args: &ArgMatches, env: &CliEnv) -> Result<Api> {
    let base = match args.get_one::<String>("url") {
        Some(url) => {
            let url = url.trim_end_matches('/');
            let url = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
            if url.ends_with("/api/v1") {
                url
            } else {
                format!("{url}/api/v1")
            }
        }
        None => (env.client)()?.base_url,
    };
    Ok(Api::new(base))
}

fn print(item: &Item, json: bool) {
    let mut out = std::io::stdout().lock();
    // A closed pipe (`fgds events | head`) ends the command, it is not an error.
    if writeln!(out, "{}", item.printed(json)).and_then(|()| out.flush()).is_err() {
        std::process::exit(0);
    }
}

/// The dictionary `--dictionary` names on the command line, for the listings: they then need no running GDS.
fn named_dictionary(args: &ArgMatches) -> std::result::Result<Option<Value>, String> {
    let Some(path) = args.try_get_one::<String>("dictionary").ok().flatten() else { return Ok(None) };
    let text = std::fs::read_to_string(path).map_err(|e| format!("dictionary file {path}: {e}"))?;
    fgds_core::serde_json::from_str(&text).map(Some).map_err(|e| format!("dictionary file {path}: {e}"))
}

/// The dictionary the names are listed from: the one `--dictionary` names, else the one of the running GDS.
async fn dictionary_of(api: &Api, given: Option<Value>) -> Result<Value> {
    if let Some(dictionary) = given {
        return Ok(dictionary);
    }
    api.check_version().await?;
    api.get("/dictionary", &[]).await
}

/// `fgds events` and `fgds channels`.
struct Records {
    name: &'static str,
    what: &'static str,
    kind: &'static str,
}

impl Subcommand for Records {
    fn command(&self) -> Command {
        Command::new(self.name)
            .about(format!("Print the {} a running GDS receives, as they arrive", self.what))
            .arg(url_arg())
            .arg(
                Arg::new("list")
                    .long("list")
                    .action(ArgAction::SetTrue)
                    .help("List the definitions of the dictionary, sorted by identifier, and exit"),
            )
            .args(filter_args())
            .arg(
                Arg::new("history")
                    .short('H')
                    .long("history")
                    .action(ArgAction::SetTrue)
                    .help("Start with what the GDS already holds, then continue with what arrives"),
            )
            .arg(
                Arg::new("no-follow")
                    .long("no-follow")
                    .action(ArgAction::SetTrue)
                    .requires("history")
                    .help("With --history: stop after what the GDS already holds"),
            )
            .arg(
                Arg::new("timeout")
                    .short('t')
                    .long("timeout")
                    .value_name("SECONDS")
                    .value_parser(fgds_core::clap::value_parser!(f64))
                    .allow_negative_numbers(true)
                    .help("Wait at most this long for the next matching item, print it and exit [default: follow until interrupted]"),
            )
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let listing = args.get_flag("list");
        let given = match named_dictionary(args) {
            Ok(given) => given.filter(|_| listing),
            Err(missing) => return Ok(not_done(if listing { "listed" } else { "read" }, missing)),
        };
        let api = api(args, env)?;
        let filter = Filter::from(args);
        let json = args.get_flag("json");
        // Zero and below mean no limit.
        let timeout = args.get_one::<f64>("timeout").copied().filter(|t| *t > 0.0);
        runtime()?.block_on(async {
            if listing {
                let dictionary = dictionary_of(&api, given).await?;
                let definitions =
                    if self.kind == "fprime.event" { items::events(&dictionary) } else { items::channels(&dictionary) };
                definitions.iter().filter(|item| filter.accepts(item, json)).for_each(|item| print(item, json));
                return Ok(ExitCode::SUCCESS);
            }
            api.check_version().await?;
            // The cursor is read before anything else, so nothing falls between the
            // stored records and the live ones (API-6).
            let cursor = api.get("/cursor", &[]).await?;
            let now =
                api::seq(&cursor["seq"]).ok_or_else(|| Error::Other("the cursor has no sequence number".into()))?;
            let mut after = if args.get_flag("history") { 0 } else { now };
            let dictionary = if json { Some(api.get("/dictionary", &[]).await?) } else { None };
            let plain = match &dictionary {
                Some(dictionary) if self.kind == "fprime.channel" => items::plain_channels(dictionary),
                _ => Default::default(),
            };
            let shown = std::cell::Cell::new(false);
            let mut on_record = |record: &Value| {
                let item = Item::from_record(record, &plain, dictionary.as_ref());
                let accepted = filter.accepts(&item, json);
                if accepted {
                    shown.set(true);
                    print(&item, json);
                }
                accepted
            };
            if args.get_flag("no-follow") {
                api.history(self.kind, &mut after, Some(now), &mut on_record).await?;
                return Ok(ExitCode::SUCCESS);
            }
            let follow = api.follow(self.kind, &mut after, timeout.is_some(), &mut on_record);
            let limit = async {
                match timeout {
                    Some(seconds) => tokio::time::sleep(Duration::from_secs_f64(seconds)).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                result = follow => result.map(|()| ExitCode::SUCCESS),
                // Nothing in time is a failure, so that a script can wait for an event.
                () = limit => Ok(if shown.get() { ExitCode::SUCCESS } else { ExitCode::FAILURE }),
                () = fgds_core::interrupt() => Ok(ExitCode::SUCCESS),
            }
        })
    }
}

/// The commands of `dictionary` that the filters accept.
fn list_commands(args: &ArgMatches, dictionary: &Value) -> ExitCode {
    let filter = Filter::from(args);
    let json = args.get_flag("json");
    items::commands(dictionary).iter().filter(|item| filter.accepts(item, json)).for_each(|item| print(item, json));
    ExitCode::SUCCESS
}

/// `fgds commands`: the same listing as `command-send --list`.
struct Commands;

impl Subcommand for Commands {
    fn command(&self) -> Command {
        Command::new("commands")
            .about("List the commands of a running GDS's dictionary, sorted by opcode")
            .arg(url_arg())
            .args(filter_args())
    }

    fn honours(&self) -> &'static [&'static str] {
        &["bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let api = api(args, env)?;
        runtime()?.block_on(async {
            api.check_version().await?;
            Ok(list_commands(args, &api.get("/dictionary", &[]).await?))
        })
    }
}

/// `fgds command-send`.
struct CommandSend;

impl Subcommand for CommandSend {
    fn command(&self) -> Command {
        Command::new("command-send")
            .about("Send a command through a running GDS")
            .arg(url_arg())
            .arg(
                Arg::new("command-name")
                    .required_unless_present("list")
                    .help("Full name of the command, \"<component>.<name>\""),
            )
            .arg(
                Arg::new("arguments")
                    .long("arguments")
                    .value_name("ARGUMENT")
                    .num_args(0..)
                    .action(ArgAction::Append)
                    .allow_negative_numbers(true)
                    .help("The arguments of the command, in order; one that starts with a dash and is no number is written --arguments=VALUE"),
            )
            .arg(
                Arg::new("dry-run")
                    .long("dry-run")
                    .action(ArgAction::SetTrue)
                    .help("Check and encode the command, print its packet in hexadecimal, send nothing"),
            )
            .arg(
                Arg::new("list")
                    .long("list")
                    .action(ArgAction::SetTrue)
                    .help("List the commands instead (same as `commands`)"),
            )
            .args(filter_args())
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let listing = args.get_flag("list");
        let name = args.get_one::<String>("command-name");
        let given = match named_dictionary(args) {
            Ok(given) => given.filter(|_| listing),
            Err(missing) => return Ok(not_done(if listing { "listed" } else { "sent" }, missing)),
        };
        let api = api(args, env)?;
        let json = args.get_flag("json");
        let arguments: Vec<&String> = args.get_many::<String>("arguments").map(Iterator::collect).unwrap_or_default();
        let dry_run = args.get_flag("dry-run");
        let unknown = |name: &str, commands: &[Item]| {
            fgds_core::err!("{name} is not a known command");
            let similar: Vec<String> = items::similar(name, commands).iter().map(|s| format!("'{s}'")).collect();
            if !similar.is_empty() {
                fgds_core::err!("Similar known commands: [{}]", similar.join(", "));
            }
            ExitCode::FAILURE
        };
        runtime()?.block_on(async {
            let Some(name) = name.filter(|_| !listing) else {
                return Ok(list_commands(args, &dictionary_of(&api, given).await?));
            };
            api.check_version().await?;
            let body = json!({"name": name, "args": arguments});
            let query: &[(&str, &str)] = if dry_run { &[("dry_run", "true")] } else { &[] };
            match api.post("/commands", query, &body).await {
                Ok(answer) if json && dry_run => println!("{answer}"),
                Ok(answer) if dry_run => {
                    fgds_core::out!("{}", api::hex(answer["packet"].as_str().unwrap_or_default())?)
                }
                Ok(_) => {}
                Err(error) => {
                    let dictionary = api.get("/dictionary", &[]).await?;
                    let commands = items::commands(&dictionary);
                    let Some(command) = commands.iter().find(|c| c.name == *name) else {
                        return Ok(unknown(name, &commands));
                    };
                    fgds_core::err!("{error}");
                    eprint!("{}", command.printed(false));
                    return Ok(ExitCode::FAILURE);
                }
            }
            Ok(ExitCode::SUCCESS)
        })
    }
}

/// The bytes a text of hexadecimal digits names: `0x` before a group and the spaces between groups are
/// left out, as in `0xDEADBEEF`, `de ad be ef`, `0xDE 0xAD`.
fn hex_bytes(text: &str) -> std::result::Result<Vec<u8>, String> {
    let digits: String =
        text.split_whitespace().map(|group| group.trim_start_matches("0x").trim_start_matches("0X")).collect();
    if let Some(other) = digits.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(format!("{other:?} is not a hexadecimal digit"));
    }
    if digits.len() & 1 == 1 {
        return Err(format!("{} hexadecimal digits are not a whole number of bytes", digits.len()));
    }
    Ok((0..digits.len()).step_by(2).map(|at| u8::from_str_radix(&digits[at..at + 2], 16).unwrap_or_default()).collect())
}

/// `fgds send-raw`: bytes sent as one packet, framed and nothing else.
struct SendRaw;

impl Subcommand for SendRaw {
    fn command(&self) -> Command {
        Command::new("send-raw")
            .about("Send bytes through a running GDS as one packet: framed, not serialized any further")
            .arg(url_arg())
            .arg(
                Arg::new("verbose")
                    .long("verbose")
                    .action(ArgAction::SetTrue)
                    .help("Print the bytes in hexadecimal before they are sent"),
            )
            .arg(Arg::new("bin-path").long("bin-path").value_name("PATH").help("File holding the bytes to send"))
            .group(fgds_core::clap::ArgGroup::new("bytes").args(["bin-path", "hex-string"]).required(true))
            .arg(
                Arg::new("hex-string")
                    .long("hex-string")
                    .value_name("HEX")
                    .help("The bytes to send in hexadecimal (0xDEADBEEF, deadbeef, \"de ad be ef\")"),
            )
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let data = match (args.get_one::<String>("bin-path"), args.get_one::<String>("hex-string")) {
            (None, None) | (Some(_), Some(_)) => unreachable!("clap requires one of the two"),
            (Some(path), None) => std::fs::read(path).map_err(|e| format!("binary file {path}: {e}")),
            (None, Some(text)) => hex_bytes(text).map_err(|e| format!("hex string {text:?}: {e}")),
        };
        // No bytes is no packet.
        let data = match data {
            Ok(data) if data.is_empty() => return Ok(not_done("sent", "there are no bytes to send")),
            Ok(data) => data,
            Err(missing) => return Ok(not_done("sent", missing)),
        };
        let api = api(args, env)?;
        runtime()?.block_on(async {
            api.check_version().await?;
            if let Some(path) = args.get_one::<String>("bin-path") {
                fgds_core::out!("Read {} bytes from {path}", data.len());
            }
            let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
            if args.get_flag("verbose") {
                fgds_core::out!("Sending {} bytes of raw data: {hex}", data.len());
            }
            api.post("/raw", &[], &json!({"data": api::base64(&data)})).await?;
            Ok(ExitCode::SUCCESS)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hexadecimal_text_names_bytes() {
        assert_eq!(hex_bytes("0xDEADBEEF"), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(hex_bytes(" de ad 0Xbe 0xef "), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(hex_bytes(""), Ok(Vec::new()));
        for text in ["abc", "zz", "100x20", "-1", "0x0"] {
            assert!(hex_bytes(text).is_err(), "{text}");
        }
    }
}
