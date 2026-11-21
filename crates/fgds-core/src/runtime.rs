use crate::{
    spawn_supervised, Config, Error, Link, PluginConfig, Records, Registry, Result, Router, RunInfo, Status,
    StatusHandle, StorageConfig, Uplink, VERSION,
};
use fgds_dictionary::Dictionary;
use std::sync::Arc;
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
    router: Arc<Router>,
    stop: watch::Sender<bool>,
}

impl Core {
    /// Open storage and begin a run, start the pipeline and the handlers. The link stays
    /// down until `start_link`, which the caller runs last, after the server shell is up.
    pub async fn start(registry: Registry, config: Config, dictionary: Dictionary) -> Result<Core> {
        registry.check()?;
        let status = Status::default();
        let dictionary = Arc::new(dictionary);
        let plugins: Vec<String> = registry.manifests.iter().map(|m| m.name.to_string()).collect();

        let (plugin, storage) = named(&registry.storages, "storage", &config.storage)?.clone();
        let run = storage
            .open(&StorageConfig {
                options: config.plugin(&plugin),
                kinds: registry.kinds.clone(),
                gds_version: VERSION.to_string(),
                dictionary_version: dictionary.version().to_string(),
                plugins: plugins.clone(),
            })
            .await?;
        let records = Records::start(storage, &run, &registry.kinds, status.clone());

        let layer_names = framing_layers(&registry, &config)?;
        let mut layers = Vec::new();
        for name in &layer_names {
            let (plugin, layer) = named(&registry.framing_layers, "framing layer", name)?;
            layers.push((layer.clone(), config.plugin(plugin)));
        }
        let link = Link::new(layers, status.clone());
        let uplink = Uplink::new(&registry.encoders, dictionary.clone(), link.clone(), records.clone(), run.run);
        named(&registry.adapters, "adapter", &config.comm)?;

        let core = Core {
            router: Arc::new(Router::new(&registry.decoders, &dictionary)),
            registry: Arc::new(registry),
            config: Arc::new(config),
            dictionary,
            records,
            uplink,
            link,
            status,
            run,
            stop: watch::channel(false).0,
        };
        for name in &plugins {
            core.status.handle(name).set_state("registered");
        }
        for (plugin, factory) in &core.registry.handlers {
            let ctx = core.context(plugin);
            let handler = factory(&ctx.config);
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
            config: self.config.plugin(plugin),
            shutdown: ShutdownToken(self.stop.subscribe()),
        }
    }

    /// Start the adapter: the last step of startup.
    pub fn start_link(&self) {
        let (plugin, factory) = self.registry.adapters[&self.config.comm].clone();
        let options = self.config.plugin(&plugin);
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
        self.records.flush().await;
        self.registry.storages[&self.config.storage].1.close().await
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
