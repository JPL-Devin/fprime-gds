//! The command line against a running GDS (CLI-3): `events`, `channels`, `commands`
//! and `command-send`, the functions of `fprime-cli`. Everything goes through the
//! public HTTP and WebSocket API (CLI-2); nothing here reads the GDS's own state.

mod api;
mod items;

use api::Api;
use fgds_core::argparse::{Kind, Nargs, Opt, Parser};
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
        reg.add_subcommand(Arc::new(CommandSend))
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
                    .short('l')
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
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
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
            api.check_version().await?;
            if args.get_flag("list") {
                let dictionary = api.get("/dictionary", &[]).await?;
                let definitions =
                    if self.kind == "fprime.event" { items::events(&dictionary) } else { items::channels(&dictionary) };
                definitions.iter().filter(|item| filter.accepts(item, json)).for_each(|item| print(item, json));
                return Ok(ExitCode::SUCCESS);
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

/// `fgds commands`: what `fprime-cli command-send --list` prints.
struct Commands;

impl Subcommand for Commands {
    fn command(&self) -> Command {
        Command::new("commands")
            .about("List the commands of a running GDS's dictionary, sorted by opcode")
            .arg(url_arg())
            .args(filter_args())
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let api = api(args, env)?;
        let filter = match Filter::from(args) {
            Ok(filter) => filter,
            Err(error) => return Ok(refused(&error)),
        };
        let json = args.get_flag("json");
        runtime()?.block_on(async {
            api.check_version().await?;
            let dictionary = api.get("/dictionary", &[]).await?;
            items::commands(&dictionary)
                .iter()
                .filter(|item| filter.accepts(item, json))
                .for_each(|item| print(item, json));
            Ok(ExitCode::SUCCESS)
        })
    }
}

/// `fgds command-send`.
struct CommandSend;

/// The options of `fprime-cli command-send` (fprime-gds 4.4.0), in the order of its parser, and the two
/// `fgds` adds. Those that set up the Python GDS itself are read and have no effect here.
fn command_send_options() -> Vec<Opt> {
    use Kind::{Float, Int, Str};
    use Nargs::{Exactly, Flag, One, OneOrMore, ZeroOrMore};
    const LEVELS: Kind = Kind::Choice(&["DEBUG", "INFO", "WARNING", "ERROR"]);
    vec![
        Opt::new(&["-h", "--help"], Flag, Str),
        Opt::new(&["--no-zmq"], Flag, Str).ignored(),
        Opt::new(&["--zmq-transport"], Exactly(2), Str).ignored(),
        Opt::new(&["--tts-port"], One, Int).ignored(),
        Opt::new(&["--tts-addr"], One, Str).ignored(),
        Opt::new(&["-l", "--logs"], One, Str),
        Opt::new(&["--log-directly"], Flag, Str).ignored(),
        Opt::new(&["--log-to-stdout"], Flag, Str).ignored(),
        Opt::new(&["--log-level-gds"], One, LEVELS).ignored(),
        Opt::new(&["--disable-data-logging"], Flag, Str).ignored(),
        Opt::new(&["--log-prefix"], One, Str).ignored(),
        Opt::new(&["--no-clear-history"], Flag, Str).ignored(),
        Opt::new(&["-d", "--deployment"], One, Str).ignored(),
        Opt::new(&["--dictionary"], One, Str),
        Opt::new(&["--packet-spec"], One, Str).ignored(),
        Opt::new(&["--packet-set-name"], One, Str).ignored(),
        Opt::new(&["--hash-file"], One, Str).ignored(),
        Opt::new(&["--file-storage-directory"], One, Str).ignored(),
        Opt::new(&["--file-uplink-cooldown"], One, Float).ignored(),
        Opt::new(&["--file-uplink-chunk-size"], One, Int).ignored(),
        Opt::new(&["--arguments"], ZeroOrMore, Str),
        Opt::new(&["--list"], Flag, Str),
        Opt::new(&["-i", "--ids"], OneOrMore, Int),
        Opt::new(&["-c", "--components"], OneOrMore, Str),
        Opt::new(&["-s", "--search"], One, Str),
        Opt::new(&["-j", "--json"], Flag, Str),
        Opt::new(&["--url"], One, Str).exact(),
        Opt::new(&["--dry-run"], Flag, Str).exact(),
    ]
}

impl Subcommand for CommandSend {
    fn command(&self) -> Command {
        Command::new("command-send")
            .about("Send a command through a running GDS")
            .after_help(
                "The command line is read as `fprime-cli command-send` reads it: its options are accepted \
                 with the same spelling, also shortened (`--arg`, `-i5`, `-l LOGS`), and those that set up \
                 the Python GDS have no effect.",
            )
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
    }

    fn argparse(&self) -> Option<Parser> {
        Some(Parser { options: command_send_options(), positionals: vec![(Nargs::Optional, "command-name")] })
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        if args.get_flag("list") {
            return Commands.run(args, env);
        }
        let api = api(args, env)?;
        let json = args.get_flag("json");
        let Some(name) = args.get_one::<String>("command-name") else {
            return Ok(refused("One of command-name or --list is required"));
        };
        let arguments: Vec<&String> = args.get_many::<String>("arguments").map(Iterator::collect).unwrap_or_default();
        let dry_run = args.get_flag("dry-run");
        runtime()?.block_on(async {
            api.check_version().await?;
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
                        // As fprime-cli: said on the standard output, and the exit code is 0.
                        println!("{name} is not a known command");
                        let similar: Vec<String> =
                            items::similar(name, &commands).iter().map(|s| format!("'{s}'")).collect();
                        if !similar.is_empty() {
                            println!("Similar known commands: [{}]", similar.join(", "));
                        }
                        return Ok(ExitCode::SUCCESS);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `fixtures/command-line/table.json`: what argparse of `fprime-cli command-send` makes of each
    /// command line, its message when it refuses.
    #[test]
    fn command_send_reads_its_command_line_as_argparse() {
        let table: Vec<Value> =
            fgds_core::serde_json::from_str(include_str!("../../../fixtures/command-line/table.json")).unwrap();
        let mut parser = CommandSend.argparse().unwrap();
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
                if values["json"] == true {
                    out.push("--json".to_string());
                }
                if let Some(name) = values["command_name"].as_str() {
                    out.extend(["--".to_string(), name.to_string()]);
                }
                Ok(out)
            };
            let got = parser.canonical(&tokens);
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
