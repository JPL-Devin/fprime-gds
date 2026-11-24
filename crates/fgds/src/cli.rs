//! The command line: `run`, `check` and the subcommands of the plugins (section 4.9).

use crate::GdsBuilder;
use clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::{ApiClient, CliEnv, Config, Core, Dictionary, Error, Registry, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// Used when no `--config` is given and the file exists (CLI-1).
const DEFAULT_CONFIG: &str = "fgds.toml";

/// Core options that take a value: (name, help). Each is also a key of the
/// configuration file. With `config` and `read-only` they are `fgds_core::CORE_OPTIONS`,
/// global arguments accepted before or after any subcommand.
const OPTIONS: [(&str, &str); 7] = [
    ("dictionary", "Path of the F Prime JSON dictionary"),
    ("comm", "Communication adapter"),
    ("framing", "Framing profile"),
    ("storage", "Storage"),
    ("database", "Database file of the storage [default: one per log directory]"),
    ("logs", "Directory that receives one log directory per run"),
    ("bind", "Address of the HTTP server"),
];

/// The argument of option `name` of `plugin`: `--<plugin>-<name>`.
fn option_name(plugin: &str, name: &str) -> String {
    format!("{plugin}-{name}")
}

/// The error of the command line with the control characters of the arguments written out: the error
/// of the same command line with each argument made visible first, since the message of clap has line
/// ends of its own that an argument must not add to.
fn shown_arguments(registry: &Registry, given: &[OsString], error: clap::Error) -> clap::Error {
    if !error.use_stderr() {
        return error;
    }
    let shown = |arg: &OsString| OsString::from(fgds_core::visible(&arg.to_string_lossy()).into_owned());
    let args: Vec<OsString> = given.iter().map(shown).collect();
    let again = as_argparse(registry, args).ok().map(|args| command(registry).try_get_matches_from(args));
    match again {
        Some(Err(again)) => again,
        // Not the same refusal once visible: the first line of the message alone, on one line.
        _ => {
            let text = error.to_string();
            let line = fgds_core::visible(text.lines().next().unwrap_or_default()).into_owned();
            command(registry).error(error.kind(), line.trim_start_matches("error: "))
        }
    }
}

/// The standard error for the log lines: one line per write, its control characters written out.
#[derive(Clone, Copy)]
struct VisibleStderr;

impl std::io::Write for VisibleStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let (line, end) = match text.strip_suffix('\n') {
            Some(line) => (line, "\n"),
            None => (&*text, ""),
        };
        std::io::stderr().write_all(format!("{}{end}", fgds_core::visible(line)).as_bytes())?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

pub fn run(builder: &GdsBuilder, args: Vec<OsString>) -> Result<ExitCode> {
    // Registration comes first: a conflict stops every command, and the plugins'
    // options and subcommands are part of the command line.
    let registry = builder.registry(&[])?;
    let given = args.clone();
    let args = match as_argparse(&registry, args) {
        Ok(args) => args,
        Err(error) => {
            fgds_core::err!("fgds: error: {error}");
            return Ok(ExitCode::from(2));
        }
    };
    let matches = match command(&registry).try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(e) => {
            let e = shown_arguments(&registry, &given, e);
            let _ = e.print();
            return Ok(if e.use_stderr() { ExitCode::from(2) } else { ExitCode::SUCCESS });
        }
    };
    let (name, sub) = matches.subcommand().expect("a subcommand is required");
    if let Some(subcommand) = registry.subcommands.iter().find(|s| s.command().get_name() == name) {
        if let Some(option) = not_honoured(&registry, &matches, subcommand.honours()) {
            fgds_core::err!("fgds: error: --{option} is an option of `fgds run`; `fgds {name}` cannot honour it");
            return Ok(ExitCode::from(2));
        }
    }
    let mut config = config(&matches, &registry)?;
    let replay = if name == "run" { sub.get_one::<String>("replay") } else { None };
    if let Some(database) = replay {
        config.database = Some(database.into());
        config.read_only = true;
    }
    let registry = match config.disabled_plugins.is_empty() {
        true => registry,
        false => builder.registry(&config.disabled_plugins)?,
    };
    config.check_plugins(&registry)?;
    match name {
        "run" => serve(registry, config),
        "check" => check(&registry, &config),
        _ => {
            let subcommand = registry.subcommands.iter().find(|s| s.command().get_name() == name);
            let dictionary = || Ok(Arc::new(load_dictionary(&config)?));
            let client = || Ok(ApiClient { base_url: format!("http://{}/api/v1", config.bind) });
            let storage = || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(Error::Io)?;
                runtime.block_on(fgds_core::open_read_only(&registry, &config))
            };
            let env = CliEnv {
                config: &config,
                registry: &registry,
                dictionary: &dictionary,
                client: &client,
                storage: &storage,
            };
            subcommand.expect("clap accepted the subcommand").run(sub, &env)
        }
    }
}

/// The command line of a subcommand that stands for a command of `fprime-cli` (`Subcommand::argparse`),
/// read as argparse reads it and written again without anything clap could read otherwise. The options
/// of the core and of the plugins are accepted there too, written in full.
fn as_argparse(registry: &Registry, args: Vec<OsString>) -> std::result::Result<Vec<OsString>, String> {
    use fgds_core::argparse::{Kind, Nargs, Opt};
    let flags = ["read-only", "no-page"];
    let mut globals: Vec<String> =
        ["config"].iter().chain(OPTIONS.iter().map(|(name, _)| name)).map(|n| n.to_string()).collect();
    for manifest in &registry.manifests {
        globals.extend(manifest.options.iter().map(|spec| option_name(manifest.name, spec.name)));
    }
    // The subcommand is the first token that is not an option of the core or its value.
    let mut at = 1;
    while let Some(token) = args.get(at).and_then(|a| a.to_str()) {
        match token.strip_prefix("--") {
            Some(name) if name.contains('=') || flags.contains(&name) => at += 1,
            Some(name) if globals.iter().any(|g| g == name) => at += 2,
            _ => break,
        }
    }
    let name = args.get(at).and_then(|a| a.to_str());
    let subcommand = registry.subcommands.iter().find(|s| Some(s.command().get_name()) == name);
    let Some(mut parser) = subcommand.and_then(|s| s.argparse()) else { return Ok(args) };
    let Some(tokens) = args[at + 1..].iter().map(|a| a.to_str().map(str::to_string)).collect::<Option<Vec<_>>>() else {
        return Ok(args);
    };
    for name in globals.iter().map(String::as_str).chain(flags) {
        let long = format!("--{name}");
        if !parser.options.iter().any(|o| o.strings.contains(&long)) {
            let nargs = if flags.contains(&name) { Nargs::Flag } else { Nargs::One };
            parser.options.push(Opt::new(&[&long], nargs, Kind::Str).exact());
        }
    }
    let mut out = args[..=at].to_vec();
    out.extend(parser.canonical(&tokens)?.into_iter().map(OsString::from));
    Ok(out)
}

/// The names of the options of the core and of the plugins, `config` apart: what configures `fgds run`.
fn run_options(registry: &Registry) -> Vec<String> {
    let mut names: Vec<String> =
        ["read-only", "no-page"].iter().chain(OPTIONS.iter().map(|(name, _)| name)).map(|n| n.to_string()).collect();
    for manifest in &registry.manifests {
        names.extend(manifest.options.iter().map(|spec| option_name(manifest.name, spec.name)));
    }
    names
}

/// The first option of `fgds run` given on the command line that the subcommand does not honour.
fn not_honoured(registry: &Registry, matches: &ArgMatches, honoured: &[&str]) -> Option<String> {
    use clap::parser::ValueSource;
    let mut levels = vec![matches];
    while let Some((_, sub)) = levels.last().and_then(|m| m.subcommand()) {
        levels.push(sub);
    }
    run_options(registry).into_iter().filter(|name| !honoured.contains(&name.as_str())).find(|name| {
        levels.iter().any(|m| {
            matches!(m.try_contains_id(name), Ok(true)) && m.value_source(name) == Some(ValueSource::CommandLine)
        })
    })
}

fn command(registry: &Registry) -> Command {
    let global = |name: &str, help: String| Arg::new(name.to_string()).long(name.to_string()).help(help).global(true);
    let mut command = Command::new("fgds")
        .about("F Prime Ground Data System")
        .version(fgds_core::VERSION)
        .subcommand_required(true)
        .arg_required_else_help(true)
        // As argparse, for every subcommand: a long option may be shortened while it names one option
        // only, and of an option given twice the last value counts.
        .infer_long_args(true)
        .args_override_self(true)
        .arg(global("config", "Configuration file [default: fgds.toml when present]".into()))
        .arg(
            global("read-only", "Open the database without writing to it; the link is not started".into())
                .action(ArgAction::SetTrue),
        )
        .arg(
            global("no-page", "Serve the API alone, without the page (as `fprime-gds -g none`)".into())
                .action(ArgAction::SetTrue),
        )
        .args(OPTIONS.iter().map(|(name, help)| global(name, help.to_string())))
        .subcommand(Command::new("run").about("Start the GDS").arg(
            Arg::new("replay").long("replay").value_name("DATABASE").help(
                "Serve the runs of a database file without a link and without writing to it; \
                 the dictionary is the one stored with its last run",
            ),
        ))
        .subcommand(Command::new("check").about("Load configuration, dictionary and plugins; print what would start"));
    for manifest in &registry.manifests {
        for spec in &manifest.options {
            let help = match spec.default {
                Some(default) => format!("{} [default: {default}]", spec.help),
                None => spec.help.to_string(),
            };
            command = command.arg(global(&option_name(manifest.name, spec.name), help));
        }
    }
    let run_options = run_options(registry);
    for subcommand in &registry.subcommands {
        let refused: Vec<&String> =
            run_options.iter().filter(|n| !subcommand.honours().contains(&n.as_str())).collect();
        command = command.subcommand(without(subcommand.command(), &refused));
    }
    command
}

/// The subcommand with the options of `fgds run` it does not honour left out of its help, at every level.
/// They are still read, to be refused by name (`not_honoured`).
fn without(command: Command, refused: &[&String]) -> Command {
    let own: Vec<String> = command.get_arguments().map(|a| a.get_id().to_string()).collect();
    let flags = ["read-only", "no-page"];
    let hidden = refused.iter().filter(|name| !own.contains(name)).map(|name| {
        let arg = Arg::new(name.to_string()).long(name.to_string()).hide(true);
        if flags.contains(&name.as_str()) {
            arg.action(ArgAction::SetTrue)
        } else {
            arg
        }
    });
    let command = command.args(hidden.collect::<Vec<_>>());
    let nested: Vec<String> = command.get_subcommands().map(|c| c.get_name().to_string()).collect();
    nested.iter().fold(command, |command, name| command.mut_subcommand(name, |c| without(c, refused)))
}

/// The configuration file, then the command-line options over it.
fn config(matches: &ArgMatches, registry: &Registry) -> Result<Config> {
    let path = match matches.get_one::<String>("config") {
        Some(path) => Some(PathBuf::from(path)),
        None => Some(PathBuf::from(DEFAULT_CONFIG)).filter(|p| p.exists()),
    };
    let mut config = match &path {
        Some(path) => {
            fgds_core::err!("configuration: {}", path.display());
            Config::load(path)?
        }
        None => Config::default(),
    };
    let given = |name: &str| matches.get_one::<String>(name).cloned();
    if let Some(dictionary) = given("dictionary") {
        config.dictionary = dictionary.into();
    }
    if let Some(database) = given("database") {
        config.database = Some(database.into());
    }
    if let Some(logs) = given("logs") {
        config.logs = Some(logs.into());
    }
    if matches.get_flag("read-only") {
        config.read_only = true;
    }
    if matches.get_flag("no-page") {
        config.no_page = true;
    }
    for (name, target) in [
        ("comm", &mut config.comm),
        ("framing", &mut config.framing),
        ("storage", &mut config.storage),
        ("bind", &mut config.bind),
    ] {
        if let Some(value) = given(name) {
            *target = value;
        }
    }
    for manifest in &registry.manifests {
        for spec in &manifest.options {
            if let Some(value) = given(&option_name(manifest.name, spec.name)) {
                config.plugins.entry(manifest.name.to_string()).or_default().set(spec.name, &value);
            }
        }
    }
    Ok(config)
}

fn load_dictionary(config: &Config) -> Result<Dictionary> {
    if config.dictionary == Path::new("") {
        return Err(Error::Config("no dictionary: give --dictionary or set `dictionary` in the configuration".into()));
    }
    Dictionary::load(&config.dictionary).map_err(|e| Error::Config(e.to_string()))
}

/// `fgds check`: everything `run` does before it opens storage, the server or the link.
fn check(registry: &Registry, config: &Config) -> Result<ExitCode> {
    let dictionary = load_dictionary(config)?;
    // What `run` does, by the function `run` calls, so a configuration `run` refuses does not pass here.
    let fgds_core::Preflight { layer_names: layers, decoders, .. } =
        fgds_core::preflight(registry, config, &dictionary)?;

    fgds_core::out!("dictionary: {} (version {})", config.dictionary.display(), dictionary.version());
    let (commands, events, channels) = (dictionary.commands.len(), dictionary.events.len(), dictionary.channels.len());
    fgds_core::out!("  {commands} commands, {events} events, {channels} channels");
    fgds_core::out!("adapter: {}", config.comm);
    fgds_core::out!("framing: {}", layers.join(" > "));
    fgds_core::out!("storage: {}", config.storage);
    fgds_core::out!("server: http://{}{}", config.bind, if config.no_page { " (API only, no page)" } else { "" });
    fgds_core::out!("plugins:");
    for manifest in &registry.manifests {
        fgds_core::out!("  {} {}", manifest.name, manifest.version);
        for spec in &manifest.options {
            let value = match config.plugin_config(registry, manifest.name)?.get(spec.name) {
                Some(_) if spec.secret => "<set>".to_string(),
                Some(value) => value,
                None => "<unset>".to_string(),
            };
            fgds_core::out!("    --{} = {value}", option_name(manifest.name, spec.name));
        }
    }
    let kinds: Vec<_> = registry.kinds.iter().map(|k| k.id.as_str()).collect();
    fgds_core::out!("record kinds: {}", kinds.join(", "));
    fgds_core::out!("decoders: {}", decoders.len());
    let routes: Vec<_> = registry.routes.iter().map(|(_, r)| format!("/api/v1/{}", r.prefix)).collect();
    fgds_core::out!("plugin routes: {}", routes.join(", "));
    let tabs: Vec<_> = registry.tabs.iter().map(|t| t.tab_id).collect();
    fgds_core::out!("tabs: {}", tabs.join(", "));
    let subcommands: Vec<_> = registry.subcommands.iter().map(|s| s.command().get_name().to_string()).collect();
    fgds_core::out!("subcommands: {}", subcommands.join(", "));
    fgds_core::out!("check passed: nothing was started");
    Ok(ExitCode::SUCCESS)
}

/// `fgds run`: the start order of section 4.4, then serve until interrupted.
fn serve(registry: Registry, config: Config) -> Result<ExitCode> {
    // No colours: the lines hold no escape sequence, of the program or of the data.
    tracing_subscriber::fmt().with_ansi(false).with_writer(|| VisibleStderr).init();
    let runtime = tokio::runtime::Runtime::new().map_err(Error::Io)?;
    runtime.block_on(async {
        // An old database is shown with the dictionary it was recorded with (STO-5).
        let dictionary = match config.read_only && config.dictionary == Path::new("") {
            true => fgds_core::stored_dictionary(&registry, &config).await?,
            false => load_dictionary(&config)?,
        };
        let core = Arc::new(Core::start(registry, config, dictionary).await?);
        let listener = fgds_shell::bind(&core).await?;
        let what = if core.config.no_page { " (API only, no page)" } else { "" };
        fgds_core::err!("listening on http://{}{what}", listener.local_addr().map_err(Error::Io)?);
        core.start_link()?;
        let served = fgds_shell::serve(core.clone(), listener, interrupted()).await;
        core.stop().await?;
        served.map(|()| ExitCode::SUCCESS)
    })
}

/// Completes on Ctrl-C or, on Unix, SIGTERM.
async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                () = fgds_core::interrupt() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    fgds_core::interrupt().await;
}
