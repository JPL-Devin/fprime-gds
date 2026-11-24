//! The command line against a running GDS (CLI-3): `events`, `channels`, `commands`
//! and `command-send`, the functions of `fprime-cli`. Everything goes through the
//! public HTTP and WebSocket API (CLI-2); nothing here reads the GDS's own state.

mod api;
mod items;

use api::Api;
use fgds_core::argparse::{Kind, Nargs, Opt, Parser};
use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::gds_options::{self, has_packet_set, not_done, READ_AS_FPRIME_CLI};
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

/// A command line that argparse of `fprime-cli` refuses: its last line and its exit code.
fn refused(error: &str) -> ExitCode {
    eprintln!("fgds: error: {error}");
    ExitCode::from(2)
}

fn filter_args() -> [Arg; 4] {
    [
        Arg::new("ids")
            .short('i')
            .long("ids")
            .value_name("ID")
            .num_args(1..)
            .action(ArgAction::Append)
            .allow_negative_numbers(true)
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
            .help("Only items whose printed line contains this"),
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
    if writeln!(out, "{}", item.line(json)).and_then(|()| out.flush()).is_err() {
        std::process::exit(0);
    }
}

/// `--list` and the filters of `fprime-cli`, after its GDS options.
fn search_options() -> Vec<Opt> {
    use Nargs::{Flag, One, OneOrMore};
    vec![
        Opt::new(&["--list"], Flag, Kind::Str),
        Opt::new(&["-i", "--ids"], OneOrMore, Kind::Int),
        Opt::new(&["-c", "--components"], OneOrMore, Kind::Str),
        Opt::new(&["-s", "--search"], One, Kind::Str),
    ]
}

/// The options of `fprime-cli command-send`, then the two `fgds` adds.
fn command_send_options() -> Vec<Opt> {
    let mut options = gds_options::options();
    options.push(Opt::new(&["--arguments"], Nargs::ZeroOrMore, Kind::Str));
    options.extend(search_options());
    options.push(Opt::new(&["-j", "--json"], Nargs::Flag, Kind::Str));
    options.push(Opt::new(&["--url"], Nargs::One, Kind::Str).exact());
    options.push(Opt::new(&["--dry-run"], Nargs::Flag, Kind::Str).exact());
    options
}

/// The options of `fprime-cli events` and `fprime-cli channels`, then the three `fgds` adds.
fn send_raw_options() -> Vec<Opt> {
    let mut options = gds_options::options();
    options.push(Opt::new(&["--verbose"], Nargs::Flag, Kind::Str));
    options.push(Opt::new(&["--bin-path"], Nargs::One, Kind::Str));
    options.push(Opt::new(&["--hex-string"], Nargs::One, Kind::Str));
    options.push(Opt::new(&["--url"], Nargs::One, Kind::Str).exact());
    options
}

fn records_options() -> Vec<Opt> {
    let mut options = gds_options::options();
    options.extend(search_options());
    options.push(Opt::new(&["-t", "--timeout"], Nargs::One, Kind::Float));
    options.push(Opt::new(&["-j", "--json"], Nargs::Flag, Kind::Str));
    options.push(Opt::new(&["--url"], Nargs::One, Kind::Str).exact());
    options.push(Opt::new(&["-H", "--history"], Nargs::Flag, Kind::Str).exact());
    options.push(Opt::new(&["--no-follow"], Nargs::Flag, Kind::Str).exact());
    options
}

/// The dictionary the names are looked up in: the one the options name, else the one of the running GDS,
/// which then has to have the packet set that is named.
async fn dictionary_of(
    api: &Api,
    args: &ArgMatches,
    given: Option<Value>,
) -> Result<std::result::Result<Value, (u8, String)>> {
    if let Some(dictionary) = given {
        return Ok(Ok(dictionary));
    }
    api.check_version().await?;
    let dictionary = api.get("/dictionary", &[]).await?;
    Ok(match args.try_get_one::<String>("packet-set-name").ok().flatten() {
        Some(set) => has_packet_set(&dictionary, set).map(|()| dictionary),
        None => Ok(dictionary),
    })
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
            .after_help(READ_AS_FPRIME_CLI)
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
                    .allow_negative_numbers(true)
                    .help("Wait at most this long for the next matching item, print it and exit [default: follow until interrupted]"),
            )
            .args(gds_options::args())
    }

    fn argparse(&self) -> Option<Parser> {
        Some(Parser { options: records_options(), positionals: Vec::new() })
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "logs", "bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let listing = args.get_flag("list");
        let given = match gds_options::hold(args, env, listing) {
            Ok(given) => given,
            Err(missing) => return Ok(not_done(if listing { "listed" } else { "read" }, missing)),
        };
        let api = api(args, env)?;
        let filter = match Filter::from(args) {
            Ok(filter) => filter,
            Err(error) => return Ok(refused(&error)),
        };
        let json = args.get_flag("json");
        // As argparse `type=float`: its message for a text that is no number; zero and below mean no limit.
        let timeout = match args.get_one::<String>("timeout").map(|text| (text, text.trim().parse::<f64>())) {
            Some((text, Err(_))) => {
                return Ok(refused(&format!("argument -t/--timeout: invalid float value: '{text}'")));
            }
            Some((_, Ok(seconds))) => Some(seconds).filter(|t| *t > 0.0),
            None => None,
        };
        runtime()?.block_on(async {
            if listing {
                let dictionary = match dictionary_of(&api, args, given).await? {
                    Ok(dictionary) => dictionary,
                    Err(missing) => return Ok(not_done("listed", missing)),
                };
                let definitions =
                    if self.kind == "fprime.event" { items::events(&dictionary) } else { items::channels(&dictionary) };
                definitions.iter().filter(|item| filter.accepts(item, json)).for_each(|item| print(item, json));
                return Ok(ExitCode::SUCCESS);
            }
            api.check_version().await?;
            if given.is_none() && args.contains_id("packet-set-name") {
                if let Err(missing) = dictionary_of(&api, args, None).await? {
                    return Ok(not_done("read", missing));
                }
            }
            // The cursor is read before anything else, so nothing falls between the
            // stored records and the live ones (API-6).
            let cursor = api.get("/cursor", &[]).await?;
            let now =
                api::seq(&cursor["seq"]).ok_or_else(|| Error::Other("the cursor has no sequence number".into()))?;
            let mut after = if args.get_flag("history") { 0 } else { now };
            let plain = if json && self.kind == "fprime.channel" {
                items::plain_channels(&api.get("/dictionary", &[]).await?)
            } else {
                Default::default()
            };
            let mut on_record = |record: &Value| {
                let item = Item::from_record(record, &plain);
                let accepted = filter.accepts(&item, json);
                if accepted {
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
                // `fprime-cli` ends with exit code 0 when nothing came in time.
                () = limit => Ok(ExitCode::SUCCESS),
                () = fgds_core::interrupt() => Ok(ExitCode::SUCCESS),
            }
        })
    }
}

/// The commands of `dictionary` that the filters accept, as `fprime-cli command-send --list` prints them.
fn list_commands(args: &ArgMatches, dictionary: &Value) -> ExitCode {
    let filter = match Filter::from(args) {
        Ok(filter) => filter,
        Err(error) => return refused(&error),
    };
    let json = args.get_flag("json");
    items::commands(dictionary).iter().filter(|item| filter.accepts(item, json)).for_each(|item| print(item, json));
    ExitCode::SUCCESS
}

/// `fgds commands`: what `fprime-cli command-send --list` prints.
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
            .after_help(READ_AS_FPRIME_CLI)
            .arg(url_arg())
            .arg(Arg::new("command-name").help("Full name of the command, \"<component>.<name>\""))
            .arg(
                Arg::new("arguments")
                    .long("arguments")
                    .value_name("ARGUMENT")
                    .num_args(0..)
                    .action(ArgAction::Append)
                    .help("The arguments of the command, in order"),
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
            .args(gds_options::args())
    }

    fn argparse(&self) -> Option<Parser> {
        Some(Parser { options: command_send_options(), positionals: vec![(Nargs::Optional, "command-name")] })
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "logs", "bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let listing = args.get_flag("list");
        let name = args.get_one::<String>("command-name");
        if name.is_none() && !listing {
            return Ok(refused("One of command-name or --list is required"));
        }
        let given = match gds_options::hold(args, env, listing) {
            Ok(given) => given,
            Err(missing) => return Ok(not_done(if listing { "listed" } else { "sent" }, missing)),
        };
        let api = api(args, env)?;
        let json = args.get_flag("json");
        let arguments: Vec<&String> = args.get_many::<String>("arguments").map(Iterator::collect).unwrap_or_default();
        let dry_run = args.get_flag("dry-run");
        // As fprime-cli: said on the standard output, and the exit code is 0.
        let unknown = |name: &str, commands: &[Item]| {
            println!("{name} is not a known command");
            let similar: Vec<String> = items::similar(name, commands).iter().map(|s| format!("'{s}'")).collect();
            if !similar.is_empty() {
                println!("Similar known commands: [{}]", similar.join(", "));
            }
            ExitCode::SUCCESS
        };
        runtime()?.block_on(async {
            let Some(name) = name.filter(|_| !listing) else {
                return Ok(match dictionary_of(&api, args, given).await? {
                    Ok(dictionary) => list_commands(args, &dictionary),
                    Err(missing) => not_done("listed", missing),
                });
            };
            // The name is looked up in the dictionary the options name: a command it does not have is not sent.
            if let Some(dictionary) = &given {
                let commands = items::commands(dictionary);
                if !commands.iter().any(|c| c.name == *name) {
                    return Ok(unknown(name, &commands));
                }
            }
            api.check_version().await?;
            if given.is_none() && args.contains_id("packet-set-name") {
                if let Err(missing) = dictionary_of(&api, args, None).await? {
                    return Ok(not_done("sent", missing));
                }
            }
            let body = json!({"name": name, "args": arguments});
            let query: &[(&str, &str)] = if dry_run { &[("dry_run", "true")] } else { &[] };
            match api.post("/commands", query, &body).await {
                // `fprime-cli command-send -j` prints nothing for a command that is sent.
                Ok(answer) if json && dry_run => println!("{answer}"),
                Ok(answer) if dry_run => println!("{}", api::hex(answer["packet"].as_str().unwrap_or_default())?),
                Ok(_) => {}
                Err(error) => {
                    let dictionary = api.get("/dictionary", &[]).await?;
                    let commands = items::commands(&dictionary);
                    let Some(command) = commands.iter().find(|c| c.name == *name) else {
                        return Ok(unknown(name, &commands));
                    };
                    eprintln!("{error}");
                    eprint!("{}", command.line(false));
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

/// `fgds send-raw`: bytes sent as one packet, framed and nothing else, as `fprime-cli send-raw`.
struct SendRaw;

impl Subcommand for SendRaw {
    fn command(&self) -> Command {
        Command::new("send-raw")
            .about("Send bytes through a running GDS as one packet: framed, not serialized any further")
            .after_help(READ_AS_FPRIME_CLI)
            .arg(url_arg())
            .arg(
                Arg::new("verbose")
                    .long("verbose")
                    .action(ArgAction::SetTrue)
                    .help("Print the bytes in hexadecimal before they are sent"),
            )
            .arg(Arg::new("bin-path").long("bin-path").value_name("PATH").help("File holding the bytes to send"))
            .arg(
                Arg::new("hex-string")
                    .long("hex-string")
                    .value_name("HEX")
                    .help("The bytes to send in hexadecimal (0xDEADBEEF, deadbeef, \"de ad be ef\")"),
            )
            .args(gds_options::args())
    }

    fn argparse(&self) -> Option<Parser> {
        Some(Parser { options: send_raw_options(), positionals: Vec::new() })
    }

    fn honours(&self) -> &'static [&'static str] {
        &["dictionary", "logs", "bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let data = match (args.get_one::<String>("bin-path"), args.get_one::<String>("hex-string")) {
            (Some(_), Some(_)) => {
                return Ok(refused("argument --hex-string: not allowed with argument --bin-path"));
            }
            (None, None) => return Ok(refused("one of the arguments --bin-path --hex-string is required")),
            (Some(path), None) => std::fs::read(path).map_err(|e| format!("binary file {path}: {e}")),
            (None, Some(text)) => hex_bytes(text).map_err(|e| format!("hex string {text:?}: {e}")),
        };
        // No bytes is no packet: `fprime-cli` sends an empty one for an empty file.
        let data = match data {
            Ok(data) if data.is_empty() => return Ok(not_done("sent", (1, "there are no bytes to send".into()))),
            Ok(data) => data,
            Err(missing) => return Ok(not_done("sent", (1, missing))),
        };
        if let Err(missing) = gds_options::hold(args, env, false) {
            return Ok(not_done("sent", missing));
        }
        let api = api(args, env)?;
        runtime()?.block_on(async {
            api.check_version().await?;
            if args.contains_id("packet-set-name") {
                if let Err(missing) = dictionary_of(&api, args, None).await? {
                    return Ok(not_done("sent", missing));
                }
            }
            if let Some(path) = args.get_one::<String>("bin-path") {
                println!("Read {} bytes from {path}", data.len());
            }
            let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
            if args.get_flag("verbose") {
                println!("Sending {} bytes of raw data: {hex}", data.len());
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

    /// `fixtures/command-line/table.json`: what argparse of `fprime-cli command-send` makes of each
    /// command line, its message when it refuses.
    #[test]
    fn command_send_reads_its_command_line_as_argparse() {
        reads_as_argparse(include_str!("../../../fixtures/command-line/table.json"), CommandSend.argparse().unwrap());
    }

    /// `fixtures/command-line/records.json`: the same for `fprime-cli events`, whose parser `channels` has too.
    #[test]
    fn events_and_channels_read_their_command_line_as_argparse() {
        let events = Records { name: "events", what: "events", kind: "fprime.event" };
        reads_as_argparse(include_str!("../../../fixtures/command-line/records.json"), events.argparse().unwrap());
    }

    fn reads_as_argparse(table: &str, mut parser: Parser) {
        let table: Vec<Value> = fgds_core::serde_json::from_str(table).unwrap();
        parser.options.retain(|option| option.abbreviated);
        assert!(table.len() > 6000);
        let mut wrong = Vec::new();
        for row in &table {
            let tokens: Vec<String> =
                row["tokens"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
            let expected = if let Some(error) = row["error"].as_str() {
                Err(error.to_string())
            } else if row["help"] == true {
                Ok(vec!["--help".to_string()])
            } else {
                let values = &row["values"];
                let mut out = Vec::new();
                let texts = |value: &Value| match value {
                    Value::Array(all) => {
                        all.iter().map(|v| v.as_str().map(str::to_string).unwrap_or(v.to_string())).collect()
                    }
                    Value::String(text) => vec![text.clone()],
                    _ => Vec::new(),
                };
                for (name, field) in [("logs", "logs"), ("dictionary", "dictionary"), ("arguments", "arguments")] {
                    out.extend(texts(&values[field]).iter().map(|v| format!("--{name}={v}")));
                }
                if values["is_printing_list"] == true {
                    out.push("--list".to_string());
                }
                for name in ["ids", "components", "search"] {
                    out.extend(texts(&values[name]).iter().map(|v| format!("--{name}={v}")));
                }
                // The time limit is recorded as Python writes the number; 0.0 is also its default.
                if let Some(limit) = values["timeout"].as_str().map(|text| text.parse::<f64>().unwrap()) {
                    if limit != 0.0 {
                        out.push(format!("--timeout={limit}"));
                    }
                }
                if values["json"] == true {
                    out.push("--json".to_string());
                }
                if let Some(name) = values["command_name"].as_str() {
                    out.extend(["--".to_string(), name.to_string()]);
                }
                Ok(out)
            };
            // The table has no column for the options kept only to check what they name.
            let got = parser.canonical(&tokens).map(|mut out| {
                out.retain(|text| {
                    text != "--log-directly"
                        && !["--timeout=0", "--timeout=-0"].contains(&text.as_str())
                        && !fgds_core::gds_options::NAMING.iter().any(|name| text.starts_with(&format!("--{name}=")))
                });
                out
            });
            if got != expected {
                wrong.push(format!("{tokens:?}\n  argparse {expected:?}\n  fgds     {got:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} differ:\n{}",
            wrong.len(),
            table.len(),
            wrong[..wrong.len().min(25)].join("\n")
        );
    }
}
