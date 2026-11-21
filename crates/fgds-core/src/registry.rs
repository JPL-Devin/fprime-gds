use crate::record::core_kinds;
use crate::{
    AdapterFactory, Decoder, Encoder, Error, FramingLayer, HandlerFactory, KindSpec, Manifest, PacketMatch, Plugin,
    Result, Routes, Storage, Subcommand, UiBundle,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Everything the compiled-in plugins declare, with the plugin that declared it.
/// Each `add_` call fails when the name is already taken, naming both plugins (CUS-10).
#[derive(Default)]
pub struct Registry {
    current: String,
    /// (what, name) -> owning plugin
    claims: BTreeMap<(&'static str, String), String>,
    pub manifests: Vec<Manifest>,
    pub kinds: Vec<KindSpec>,
    /// name -> (owning plugin, factory)
    pub adapters: BTreeMap<String, (String, AdapterFactory)>,
    pub framing_layers: BTreeMap<String, (String, Arc<dyn FramingLayer>)>,
    pub framing_profiles: BTreeMap<String, Vec<String>>,
    /// (owning plugin, decoder)
    pub decoders: Vec<(String, Arc<dyn Decoder>)>,
    pub encoders: Vec<Arc<dyn Encoder>>,
    /// (owning plugin, factory)
    pub handlers: Vec<(String, HandlerFactory)>,
    /// name -> (owning plugin, storage)
    pub storages: BTreeMap<String, (String, Arc<dyn Storage>)>,
    /// (owning plugin, routes)
    pub routes: Vec<(String, Routes)>,
    pub tabs: Vec<UiBundle>,
    pub subcommands: Vec<Arc<dyn Subcommand>>,
}

impl Registry {
    /// A registry holding only the core's own kinds.
    pub fn new() -> Self {
        let mut reg = Registry { current: "core".into(), ..Default::default() };
        for kind in core_kinds() {
            reg.add_kind(kind).expect("core kinds are distinct");
        }
        reg
    }

    /// Register one plugin: its name, its options, then whatever it declares.
    pub fn register(&mut self, plugin: &dyn Plugin) -> Result<()> {
        let manifest = plugin.manifest();
        self.current = manifest.name.to_string();
        self.claim("plugin", manifest.name)?;
        for option in &manifest.options {
            self.claim("argument", option.name)?;
        }
        self.manifests.push(manifest);
        plugin.register(self)
    }

    fn claim(&mut self, what: &'static str, name: &str) -> Result<()> {
        let key = (what, name.to_string());
        if let Some(owner) = self.claims.get(&key) {
            return Err(Error::Conflict(format!(
                "{what} \"{name}\" is registered by both plugin \"{owner}\" and plugin \"{}\"",
                self.current
            )));
        }
        self.claims.insert(key, self.current.clone());
        Ok(())
    }

    pub fn add_kind(&mut self, kind: KindSpec) -> Result<()> {
        self.claim("record kind", &kind.id)?;
        self.kinds.push(kind);
        Ok(())
    }

    pub fn add_adapter(&mut self, name: &str, factory: AdapterFactory) -> Result<()> {
        self.claim("adapter", name)?;
        self.adapters.insert(name.to_string(), (self.current.clone(), factory));
        Ok(())
    }

    pub fn add_framing_layer(&mut self, name: &str, layer: Arc<dyn FramingLayer>) -> Result<()> {
        self.claim("framing layer", name)?;
        let entry = (self.current.clone(), layer);
        self.framing_layers.insert(name.to_string(), entry);
        Ok(())
    }

    /// A profile is data: a name and layer names, outermost (nearest the link) first.
    pub fn add_framing_profile(&mut self, name: &str, layers: &[&str]) -> Result<()> {
        self.claim("framing profile", name)?;
        let layers = layers.iter().map(|l| l.to_string()).collect();
        self.framing_profiles.insert(name.to_string(), layers);
        Ok(())
    }

    /// Fails when another decoder can match the same packet with equal precedence.
    pub fn add_decoder(&mut self, decoder: Arc<dyn Decoder>) -> Result<()> {
        let rule = decoder.matches();
        if let Some((owner, _)) = self.decoders.iter().find(|(_, other)| ambiguous(&rule, &other.matches())) {
            return Err(Error::Conflict(format!(
                "decoder match rule {rule:?} is registered by both plugin \"{owner}\" and plugin \"{}\"",
                self.current
            )));
        }
        self.decoders.push((self.current.clone(), decoder));
        Ok(())
    }

    /// Replace the registered decoder that has the same match rule (`Gds::override_decoder`).
    pub fn override_decoder(&mut self, decoder: Arc<dyn Decoder>) -> Result<()> {
        let rule = decoder.matches();
        match self.decoders.iter_mut().find(|(_, d)| d.matches() == rule) {
            Some(slot) => {
                slot.1 = decoder;
                Ok(())
            }
            None => Err(Error::Config(format!("override_decoder: no decoder is registered with match rule {rule:?}"))),
        }
    }

    pub fn add_encoder(&mut self, encoder: Arc<dyn Encoder>) -> Result<()> {
        self.claim("request kind", encoder.request_kind())?;
        self.encoders.push(encoder);
        Ok(())
    }

    pub fn add_handler(&mut self, factory: HandlerFactory) {
        self.handlers.push((self.current.clone(), factory));
    }

    pub fn add_storage(&mut self, name: &str, storage: Arc<dyn Storage>) -> Result<()> {
        self.claim("storage", name)?;
        self.storages.insert(name.to_string(), (self.current.clone(), storage));
        Ok(())
    }

    pub fn add_routes(&mut self, routes: Routes) -> Result<()> {
        self.claim("route prefix", routes.prefix)?;
        self.routes.push((self.current.clone(), routes));
        Ok(())
    }

    pub fn add_tab(&mut self, tab: UiBundle) -> Result<()> {
        self.claim("tab", tab.tab_id)?;
        self.tabs.push(tab);
        Ok(())
    }

    pub fn add_subcommand(&mut self, subcommand: Arc<dyn Subcommand>) -> Result<()> {
        self.claim("subcommand", subcommand.command().get_name())?;
        self.subcommands.push(subcommand);
        Ok(())
    }

    /// Checks that need every plugin registered: required kinds and profile layers.
    pub fn check(&self) -> Result<()> {
        for manifest in &self.manifests {
            for kind in &manifest.requires_kinds {
                if !self.kinds.iter().any(|k| k.id == *kind) {
                    return Err(Error::Config(format!(
                        "plugin \"{}\" requires record kind \"{kind}\", which no plugin registers",
                        manifest.name
                    )));
                }
            }
        }
        for (profile, layers) in &self.framing_profiles {
            if let Some(missing) = layers.iter().find(|l| !self.framing_layers.contains_key(*l)) {
                return Err(Error::Config(format!(
                    "framing profile \"{profile}\" names layer \"{missing}\", which no plugin registers"
                )));
            }
        }
        Ok(())
    }
}

/// Two rules are ambiguous when one packet can match both and neither takes precedence:
/// same number of metadata conditions, compatible packet types, and no metadata key
/// on which they require different values.
fn ambiguous(a: &PacketMatch, b: &PacketMatch) -> bool {
    let types_overlap = match (a.packet_type, b.packet_type) {
        (Some(x), Some(y)) => x == y,
        (None, None) => true,
        // One with a type and one without differ in precedence.
        _ => return false,
    };
    let meta_overlap = a.meta.iter().all(|(key, value)| b.meta.iter().all(|(k, v)| k != key || v == value));
    a.meta.len() == b.meta.len() && types_overlap && meta_overlap
}
