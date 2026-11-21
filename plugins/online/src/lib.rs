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
            .allow_negative_numbers(true)
            .help("Only items with one of these identifiers"),
        Arg::new("components")
            .short('c')
            .long("components")
            .value_name("COMP")
            .num_args(1..)
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
                    .value_parser(fgds_core::clap::value_parser!(f64))
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
        let timeout = args.get_one::<f64>("timeout").copied().filter(|t| *t > 0.0);
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
                () = limit => Ok(ExitCode::FAILURE),
                _ = tokio::signal::ctrl_c() => Ok(ExitCode::SUCCESS),
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

impl Subcommand for CommandSend {
    fn command(&self) -> Command {
        Command::new("command-send")
            .about("Send a command through a running GDS")
            .arg(url_arg())
            .arg(Arg::new("command-name").help("Full name of the command, \"<component>.<name>\""))
            .arg(
                Arg::new("arguments")
                    .long("arguments")
                    .value_name("ARGUMENT")
                    .num_args(0..)
                    .allow_negative_numbers(true)
                    .help("The arguments of the command, in order"),
            )
            .arg(
                Arg::new("dry-run")
                    .short('n')
                    .long("dry-run")
                    .action(ArgAction::SetTrue)
                    .help("Check and encode the command, print its packet in hexadecimal, send nothing"),
            )
            .arg(
                Arg::new("list")
                    .short('l')
                    .long("list")
                    .action(ArgAction::SetTrue)
                    .help("List the commands instead (same as `commands`)"),
            )
            .args(filter_args())
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
        // argparse of fprime-cli: an argument that starts with `-` is taken as an argument only when it
        // is a negative number written with digits (`-5`, `-0.5`), so `-3.4e38` ends the command line.
        let number = |text: &str| {
            let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            match text.split_once('.') {
                Some((whole, fraction)) => (whole.is_empty() || digits(whole)) && digits(fraction),
                None => digits(text),
            }
        };
        if let Some(first) = arguments.iter().position(|a| a.strip_prefix('-').is_some_and(|rest| !number(rest))) {
            let rest: Vec<&str> = arguments[first..].iter().map(|a| a.as_str()).collect();
            return Ok(refused(&format!("unrecognized arguments: {}", rest.join(" "))));
        }
        runtime()?.block_on(async {
            api.check_version().await?;
            let body = json!({"name": name, "args": arguments});
            let query: &[(&str, &str)] = if dry_run { &[("dry_run", "true")] } else { &[] };
            match api.post("/commands", query, &body).await {
                Ok(answer) if json => println!("{answer}"),
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
