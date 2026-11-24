//! The Dictionaries tab, which has no route of its own (it reads `GET /api/v1/dictionary`), and
//! `fgds merge-dictionary`.

pub mod merge;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use fgds_core::argparse::{Kind, Nargs, Opt, Parser};
use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::{CliEnv, Manifest, Plugin, Registry, Result, StaticFiles, Subcommand, UiBundle};

static FILES: StaticFiles = StaticFiles(&[("dictionary.js", include_bytes!("../ui/dictionary.js"))]);

pub struct DictionaryTabPlugin;

impl Plugin for DictionaryTabPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "dictionary-tab", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_tab(UiBundle {
            tab_id: "dictionary",
            title: Some("Dictionaries"),
            order: 60,
            module: "dictionary.js",
            files: &FILES,
        })?;
        reg.add_subcommand(Arc::new(MergeCommand))
    }
}

struct MergeCommand;

const FLAGS: [(&str, &str); 4] = [
    ("permissive", "Only warn when the versions of the two metadata blocks differ"),
    ("prefer-primary", "On a conflict keep the first dictionary's entry and leave the other out, with a warning"),
    ("no-namespace", "A name used with two identifiers is an error (with --prefer-primary: left out), not renamed"),
    ("namespace-all", "Prefix every command, parameter, event, channel, record and container name"),
];

impl Subcommand for MergeCommand {
    fn command(&self) -> Command {
        let mut command = Command::new("merge-dictionary")
            .about("Merge two dictionaries into one (as fprime-merge-dictionary)")
            .arg(Arg::new("dictionary1").required(true).help("First dictionary"))
            .arg(Arg::new("dictionary2").required(true).help("Second dictionary"))
            .arg(Arg::new("name").long("name").help("deploymentName of the result [default: <name1>_<name2>_merged]"))
            .arg(Arg::new("output").long("output").default_value("MergedAppDictionary.json").help("Path of the result"))
            .arg(Arg::new("prefix").long("prefix").action(ArgAction::Append).help(
                "Prefix of each dictionary, in order (give it twice) [default: last part of its deploymentName]",
            ));
        for (name, help) in FLAGS {
            command = command.arg(Arg::new(name).long(name).action(ArgAction::SetTrue).help(help));
        }
        command
    }

    fn argparse(&self) -> Option<Parser> {
        let mut options = vec![
            Opt::new(&["-h", "--help"], Nargs::Flag, Kind::Str),
            Opt::new(&["--name"], Nargs::One, Kind::Str),
            Opt::new(&["--output"], Nargs::One, Kind::Str),
        ];
        options.extend(
            ["--permissive", "--prefer-primary", "--no-namespace", "--namespace-all"]
                .map(|f| Opt::new(&[f], Nargs::Flag, Kind::Str)),
        );
        options.push(Opt::new(&["--prefix"], Nargs::One, Kind::Str).append());
        Some(Parser { options, positionals: vec![(Nargs::One, "dictionary1"), (Nargs::One, "dictionary2")] })
    }

    fn run(&self, args: &ArgMatches, _env: &CliEnv) -> Result<ExitCode> {
        let usage = |problem: &str| {
            eprintln!("fgds merge-dictionary: error: {problem}");
            Ok(ExitCode::from(2))
        };
        let prefixes: Vec<String> = args.get_many::<String>("prefix").into_iter().flatten().cloned().collect();
        if args.get_flag("no-namespace") && args.get_flag("namespace-all") {
            return usage("argument --namespace-all: not allowed with argument --no-namespace");
        }
        if !prefixes.is_empty() && prefixes.len() != 2 {
            return usage("--prefix must be given exactly twice, once per dictionary");
        }
        if !prefixes.is_empty() && args.get_flag("no-namespace") {
            return usage("--prefix cannot be combined with --no-namespace");
        }
        if !prefixes.is_empty()
            && (prefixes.first() == prefixes.last() || !prefixes.iter().all(|p| merge::is_dotted_identifier(p)))
        {
            return usage("--prefix values must be distinct dotted identifiers");
        }
        let options = merge::Options {
            name: args.get_one::<String>("name").cloned(),
            permissive: args.get_flag("permissive"),
            prefer_primary: args.get_flag("prefer-primary"),
            no_namespace: args.get_flag("no-namespace"),
            namespace_all: args.get_flag("namespace-all"),
        };
        let failed = |problem: String| {
            eprintln!("[ERROR] {problem}");
            Ok(ExitCode::FAILURE)
        };
        let mut inputs = Vec::new();
        for (position, name) in ["dictionary1", "dictionary2"].into_iter().enumerate() {
            let path = args.get_one::<String>(name).expect("required");
            let data = match std::fs::read(path) {
                Ok(bytes) => match fgds_core::serde_json::from_slice(&bytes) {
                    Ok(data) => data,
                    Err(error) => return failed(format!("{path}: {error}")),
                },
                Err(error) => return failed(format!("{path}: {error}")),
            };
            inputs.push(merge::Input { path: path.clone(), data, prefix: prefixes.get(position).cloned() });
        }
        let (merged, report) = merge::merge(&inputs[0], &inputs[1], &options);
        for warning in &report.warnings {
            eprintln!("[WARNING] {warning}");
        }
        for error in &report.errors {
            eprintln!("[ERROR] {error}");
        }
        let Some(merged) = merged else {
            return failed(format!("Merge failed with {} error(s); no output written", report.errors.len()));
        };
        let output = PathBuf::from(args.get_one::<String>("output").expect("has a default"));
        match std::fs::write(&output, merge::dumps(&merged)) {
            Ok(()) => Ok(ExitCode::SUCCESS),
            Err(error) => failed(format!("{}: {error}", output.display())),
        }
    }
}
