use crate::{
    spawn_supervised, Config, Error, Link, PipelineOptions, PluginConfig, Records, Registry, Result, Router, RunInfo,
    Status, StatusHandle, Storage, StorageConfig, Timestamp, Uplink, VERSION,
};
use fgds_dictionary::Dictionary;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Tells plugin tasks to stop. Cheap to clone.
#[derive(Clone)]
pub struct ShutdownToken(watch::Receiver<bool>);

impl ShutdownToken {
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Completes when the GDS is stopping.
    pub async fn cancelled(&mut self) {
        // An error means the core is gone, which is a stop as well.
        let _ = self.0.wait_for(|stop| *stop).await;
    }
}

/// What a plugin receives when started.
#[derive(Clone)]
pub struct Context {
    pub dictionary: Arc<Dictionary>,
    pub records: Records,
    pub uplink: Uplink,
    pub status: StatusHandle,
    pub config: PluginConfig,
    pub shutdown: ShutdownToken,
}

/// The running core, in the start order of section 4.4.
pub struct Core {
    pub registry: Arc<Registry>,
    pub config: Arc<Config>,
    pub dictionary: Arc<Dictionary>,
    pub records: Records,
    pub uplink: Uplink,
    pub link: Link,
    pub status: Status,
    pub run: RunInfo,
    /// The log directory of this run (LNK-18); `None` without `logs` or when read-only.
    pub run_dir: Option<PathBuf>,
    router: Arc<Router>,
    stop: watch::Sender<bool>,
}

impl Core {
    /// Open storage and begin a run, start the pipeline and the handlers. The link stays
    /// down until `start_link`, which the caller runs last, after the server shell is up.
    ///
    /// With `config.read_only` no run begins, nothing is written and the link and the
    /// handlers are never started: the core only serves what the storage holds (STO-12).
    pub async fn start(registry: Registry, config: Config, dictionary: Dictionary) -> Result<Core> {
        registry.check()?;
        config.check_plugins(&registry)?;
        let decoders = registry.build_decoders(&config)?;
        let encoders = registry.build_encoders(&config)?;
        let status = Status::default();
        let dictionary = Arc::new(dictionary);
        let plugins: Vec<String> = registry.manifests.iter().map(|m| m.name.to_string()).collect();
        let run_dir = match (&config.logs, config.read_only) {
            (Some(logs), false) => Some(create_run_dir(logs)?),
            _ => None,
        };

        let storage = named(&registry.storages, "storage", &config.storage)?.1.clone();
        let run = storage.open(&storage_config(&registry, &config, &dictionary, run_dir.clone())).await?;
        if !config.read_only {
            warn_if_dictionary_changed(storage.as_ref(), &run, &status).await;
        }
        let options = PipelineOptions {
            commit_timeout: Duration::from_millis(config.commit_timeout_ms),
            read_only: config.read_only,
        };
        let records = Records::start_with(storage, &run, &registry.kinds, status.clone(), options);
        let raw_log = match &run_dir {
            Some(dir) => Some(std::fs::File::create(dir.join("raw.log")).map_err(Error::Io)?),
            None => None,
        };

        let layer_names = framing_layers(&registry, &config)?;
        let mut layers = Vec::new();
        for name in &layer_names {
            let (plugin, layer) = named(&registry.framing_layers, "framing layer", name)?;
            let layer = layer.with_dictionary(&dictionary).unwrap_or_else(|| layer.clone());
            layers.push((layer, config.plugin_config(&registry, plugin)?));
        }
        let link = Link::with_raw_log(layers, status.clone(), raw_log);
        let uplink = Uplink::new(encoders, dictionary.clone(), link.clone(), records.clone(), run.run);
        named(&registry.adapters, "adapter", &config.comm)?;

        let core = Core {
            router: Arc::new(Router::new(&decoders, &dictionary)),
            registry: Arc::new(registry),
            config: Arc::new(config),
            dictionary,
            records,
            uplink,
            link,
            status,
            run,
            run_dir,
            stop: watch::channel(false).0,
        };
        if core.config.read_only {
            core.link.set_enabled(false);
            core.status.set_link("disabled", None);
            return Ok(core);
        }
        for name in &plugins {
            core.status.handle(name).set_state("registered");
        }
        for (plugin, factory) in &core.registry.handlers {
            let ctx = core.context(plugin);
            let handler = factory(&ctx.config)?;
            let stream = core.records.subscribe(handler.subscription(), None);
            spawn_supervised(plugin, core.records.clone(), core.status.clone(), handler.run(ctx, stream));
        }
        Ok(core)
    }

    /// The context of one plugin: shared services with that plugin's options and status.
    pub fn context(&self, plugin: &str) -> Context {
        Context {
            dictionary: self.dictionary.clone(),
            records: self.records.clone(),
            uplink: self.uplink.clone(),
            status: self.status.handle(plugin),
            // Checked at start, so this cannot fail for a registered plugin.
            config: self.config.plugin_config(&self.registry, plugin).unwrap_or_default(),
            shutdown: ShutdownToken(self.stop.subscribe()),
        }
    }

    /// Start the adapter: the last step of startup.
    pub fn start_link(&self) {
        if self.config.read_only {
            return;
        }
        let (plugin, factory) = self.registry.adapters[&self.config.comm].clone();
        let options = self.config.plugin_config(&self.registry, &plugin).unwrap_or_default();
        self.link.start(
            move || factory(&options),
            self.router.clone(),
            self.records.clone(),
            self.dictionary.clone(),
            ShutdownToken(self.stop.subscribe()),
        );
    }

    /// Stop plugin tasks and the link, then flush and close storage (LNK-14).
    pub async fn stop(&self) -> Result<()> {
        self.stop.send_replace(true);
        self.records.stop().await;
        self.registry.storages[&self.config.storage].1.close().await
    }
}

/// The dictionary kept with the last run of the configured database: what a read-only
/// GDS displays that run with when no dictionary is given (STO-5).
pub async fn stored_dictionary(registry: &Registry, config: &Config) -> Result<Dictionary> {
    let (storage, run) = open_read_only(registry, config).await?;
    let json = storage.dictionary(run.run).await;
    storage.close().await?;
    let json = json?.ok_or_else(|| Error::Config(format!("run {} has no dictionary in the database", run.run)))?;
    let text = String::from_utf8(json).map_err(|e| Error::Config(format!("dictionary of run {}: {e}", run.run)))?;
    Dictionary::parse(&format!("run {}", run.run), &text).map_err(|e| Error::Config(e.to_string()))
}

/// Open the configured storage read-only, starting no run: the storage and its last
/// run. For offline subcommands (`CliEnv::storage`) and a read-only GDS (STO-12). The
/// caller closes the storage.
pub async fn open_read_only(registry: &Registry, config: &Config) -> Result<(Arc<dyn Storage>, RunInfo)> {
    let storage = named(&registry.storages, "storage", &config.storage)?.1.clone();
    let config = Config { read_only: true, ..config.clone() };
    let run = storage.open(&storage_config(registry, &config, &crate::testing::empty_dictionary(), None)).await?;
    Ok((storage, run))
}

fn storage_config(
    registry: &Registry,
    config: &Config,
    dictionary: &Dictionary,
    run_dir: Option<PathBuf>,
) -> StorageConfig {
    let plugin = registry.storages.get(&config.storage).map(|(plugin, _)| plugin.as_str()).unwrap_or_default();
    let dictionary_json = serde_json::to_vec(&dictionary.json).unwrap_or_default();
    StorageConfig {
        options: config.plugin_config(registry, plugin).unwrap_or_default(),
        kinds: registry.kinds.clone(),
        gds_version: VERSION.to_string(),
        dictionary_version: dictionary.version().to_string(),
        dictionary_hash: Sha256::digest(&dictionary_json).iter().map(|b| format!("{b:02x}")).collect(),
        dictionary_json,
        plugins: registry.manifests.iter().map(|m| m.name.to_string()).collect(),
        database: config.database.clone(),
        run_dir,
        read_only: config.read_only,
    }
}

/// `<logs>/<start time>`, made unique when two runs start in the same second.
fn create_run_dir(logs: &std::path::Path) -> Result<PathBuf> {
    std::fs::create_dir_all(logs).map_err(Error::Io)?;
    let started = Timestamp::now().to_iso().replace(':', "-");
    let name = started.split('.').next().unwrap_or_default().to_string();
    for attempt in 1.. {
        let dir = logs.join(if attempt == 1 { name.clone() } else { format!("{name}-{attempt}") });
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    unreachable!()
}

/// A dictionary that differs from the previous run's is a warning, not a refusal (STO-5).
async fn warn_if_dictionary_changed(storage: &dyn Storage, run: &RunInfo, status: &Status) {
    let runs = storage.runs().await.unwrap_or_default();
    let previous = runs.iter().filter(|r| r.run < run.run).max_by_key(|r| r.run);
    if let Some(previous) = previous.filter(|p| p.dictionary_hash != run.dictionary_hash) {
        let message = format!(
            "the dictionary (version {}) differs from the dictionary of run {} (version {}): \
             records of earlier runs are shown as decoded at the time",
            run.dictionary_version, previous.run, previous.dictionary_version
        );
        tracing::warn!("{message}");
        status.warn(&message);
    }
}

/// The names of the framing layers in use, outermost first.
pub fn framing_layers(registry: &Registry, config: &Config) -> Result<Vec<String>> {
    let names = match config.framing_layers.is_empty() {
        true => named(&registry.framing_profiles, "framing profile", &config.framing)?.clone(),
        false => config.framing_layers.clone(),
    };
    for name in &names {
        named(&registry.framing_layers, "framing layer", name)?;
    }
    Ok(names)
}

/// The entry called `name`, or an error listing what is available.
pub fn named<'a, T>(map: &'a std::collections::BTreeMap<String, T>, what: &str, name: &str) -> Result<&'a T> {
    map.get(name).ok_or_else(|| {
        let known: Vec<_> = map.keys().map(String::as_str).collect();
        Error::Config(format!("unknown {what} \"{name}\"; available: {}", known.join(", ")))
    })
}
