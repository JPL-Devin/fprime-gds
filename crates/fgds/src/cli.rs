//! The command line: `run`, `check` and the subcommands of the plugins (section 4.9).

use crate::GdsBuilder;
use clap::{Arg, ArgMatches, Command};
use fgds_core::{ApiClient, CliEnv, Config, Core, Dictionary, Error, Registry, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// Used when no `--config` is given and the file exists (CLI-1).
const DEFAULT_CONFIG: &str = "fgds.toml";

/// Core options: (name, help). Each is also a key of the configuration file.
const OPTIONS: [(&str, &str); 5] = [
    ("dictionary", "Path of the F Prime JSON dictionary"),
    ("comm", "Communication adapter"),
    ("framing", "Framing profile"),
    ("storage", "Storage"),
    ("bind", "Address of the HTTP server"),
];

pub fn run(builder: &GdsBuilder, args: Vec<OsString>) -> Result<ExitCode> {
    // Registration comes first: a conflict stops every command, and the plugins'
    // options and subcommands are part of the command line.
    let registry = builder.registry(&[])?;
    let matches = match command(&registry).try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(e) => {
            let _ = e.print();
            return Ok(if e.use_stderr() { ExitCode::from(2) } else { ExitCode::SUCCESS });
        }
    };
    let (name, sub) = matches.subcommand().expect("a subcommand is required");
    let config = config(&matches, sub, &registry)?;
    let registry = match config.disabled_plugins.is_empty() {
        true => registry,
        false => builder.registry(&config.disabled_plugins)?,
    };
    match name {
        "run" => serve(registry, config),
        "check" => check(&registry, &config),
        _ => {
            let subcommand = registry.subcommands.iter().find(|s| s.command().get_name() == name);
            let dictionary = || Ok(Arc::new(load_dictionary(&config)?));
            let client = || Ok(ApiClient { base_url: format!("http://{}/api/v1", config.bind) });
            let env = CliEnv { config: &config, registry: &registry, dictionary: &dictionary, client: &client };
            subcommand.expect("clap accepted the subcommand").run(sub, &env)
        }
    }
}

fn command(registry: &Registry) -> Command {
    let mut options: Vec<Arg> = OPTIONS.iter().map(|(name, help)| Arg::new(*name).long(*name).help(*help)).collect();
    for spec in registry.manifests.iter().flat_map(|m| &m.options) {
        let help = match spec.default {
            Some(default) => format!("{} [default: {default}]", spec.help),
            None => spec.help.to_string(),
        };
        options.push(Arg::new(spec.name).long(spec.name).help(help));
    }
    let config =
        Arg::new("config").long("config").global(true).help("Configuration file [default: fgds.toml when present]");
    let mut command = Command::new("fgds")
        .about("F Prime Ground Data System")
        .version(fgds_core::VERSION)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(config)
        .subcommand(Command::new("run").about("Start the GDS").args(options.clone()))
        .subcommand(
            Command::new("check")
                .about("Load configuration, dictionary and plugins; print what would start")
                .args(options),
        );
    for subcommand in &registry.subcommands {
        command = command.subcommand(subcommand.command());
    }
    command
}

/// The configuration file, then the command-line options over it.
fn config(matches: &ArgMatches, sub: &ArgMatches, registry: &Registry) -> Result<Config> {
    let path = match matches.get_one::<String>("config") {
        Some(path) => Some(PathBuf::from(path)),
        None => Some(PathBuf::from(DEFAULT_CONFIG)).filter(|p| p.exists()),
    };
    let mut config = match &path {
        Some(path) => {
            eprintln!("configuration: {}", path.display());
            Config::load(path)?
        }
        None => Config::default(),
    };
    // Plugin subcommands have their own arguments, not these options.
    let given = |name: &str| sub.try_get_one::<String>(name).ok().flatten().cloned();
    if let Some(dictionary) = given("dictionary") {
        config.dictionary = dictionary.into();
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
            if let Some(value) = given(spec.name) {
                config.plugins.entry(manifest.name.to_string()).or_default().set(spec.name, &value);
            }
        }
    }
    if let Some(unknown) = config.plugins.keys().find(|name| !registry.manifests.iter().any(|m| m.name == *name)) {
        return Err(Error::Config(format!("[plugins.{unknown}]: no plugin has this name")));
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
    let layers = fgds_core::framing_layers(registry, config)?;
    fgds_core::named(&registry.adapters, "adapter", &config.comm)?;
    fgds_core::named(&registry.storages, "storage", &config.storage)?;

    println!("dictionary: {} (version {})", config.dictionary.display(), dictionary.version());
    let (commands, events, channels) = (dictionary.commands.len(), dictionary.events.len(), dictionary.channels.len());
    println!("  {commands} commands, {events} events, {channels} channels");
    println!("adapter: {}", config.comm);
    println!("framing: {}", layers.join(" > "));
    println!("storage: {}", config.storage);
    println!("server: http://{}", config.bind);
    println!("plugins:");
    for manifest in &registry.manifests {
        println!("  {} {}", manifest.name, manifest.version);
        for spec in &manifest.options {
            let value = match (config.plugin(manifest.name).get(spec.name), spec.default) {
                (Some(_), _) if spec.secret => "<set>".to_string(),
                (Some(value), _) => value,
                (None, Some(default)) => default.to_string(),
                (None, None) => "<unset>".to_string(),
            };
            println!("    --{} = {value}", spec.name);
        }
    }
    let kinds: Vec<_> = registry.kinds.iter().map(|k| k.id.as_str()).collect();
    println!("record kinds: {}", kinds.join(", "));
    let routes: Vec<_> = registry.routes.iter().map(|(_, r)| format!("/api/v1/{}", r.prefix)).collect();
    println!("plugin routes: {}", routes.join(", "));
    let tabs: Vec<_> = registry.tabs.iter().map(|t| t.tab_id).collect();
    println!("tabs: {}", tabs.join(", "));
    let subcommands: Vec<_> = registry.subcommands.iter().map(|s| s.command().get_name().to_string()).collect();
    println!("subcommands: {}", subcommands.join(", "));
    println!("check passed: nothing was started");
    Ok(ExitCode::SUCCESS)
}

/// `fgds run`: the start order of section 4.4, then serve until interrupted.
fn serve(registry: Registry, config: Config) -> Result<ExitCode> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let dictionary = load_dictionary(&config)?;
    let runtime = tokio::runtime::Runtime::new().map_err(Error::Io)?;
    runtime.block_on(async {
        let core = Arc::new(Core::start(registry, config, dictionary).await?);
        let listener = fgds_shell::bind(&core).await?;
        eprintln!("listening on http://{}", listener.local_addr().map_err(Error::Io)?);
        core.start_link();
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
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
