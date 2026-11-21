use crate::{Error, KindSpec, Registry, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Options of one plugin: its `[plugins.<name>]` table, then command-line values.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginConfig(pub Map<String, Value>);

impl PluginConfig {
    /// The option as text, whatever its type in the configuration file.
    pub fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.0.insert(key.to_string(), Value::String(value.into()));
    }
}

/// The merged configuration: file first, command-line options over it.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub dictionary: PathBuf,
    /// Adapter name.
    pub comm: String,
    /// Framing profile name.
    pub framing: String,
    /// Explicit layer list, outermost first; overrides `framing`.
    pub framing_layers: Vec<String>,
    /// Storage name.
    pub storage: String,
    /// Database file (STO-3); without it the storage creates `<run log directory>/gds.db`.
    pub database: Option<PathBuf>,
    /// Directory holding one log directory per run (LNK-18). `None`: no log directory,
    /// so no raw log, and the storage needs `database`.
    pub logs: Option<PathBuf>,
    /// Serve an existing database without a link and without changing the file (STO-12).
    pub read_only: bool,
    /// A storage commit that takes longer puts storage in state `failed` (STO-9).
    pub commit_timeout_ms: u64,

    /// Address of the HTTP server; local by default (SEC-1).
    pub bind: String,
    pub disabled_plugins: Vec<String>,
    /// Largest accepted request body (API-7).
    pub max_body_bytes: usize,
    pub plugins: BTreeMap<String, PluginConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dictionary: PathBuf::new(),
            comm: "tcp-server".into(),
            framing: "ccsds".into(),
            framing_layers: Vec::new(),
            storage: "sqlite".into(),
            database: None,
            logs: Some("logs".into()),
            read_only: false,
            commit_timeout_ms: 2_000,
            bind: "127.0.0.1:5000".into(),
            disabled_plugins: Vec::new(),
            max_body_bytes: 1024 * 1024,
            plugins: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let at = |e: &dyn std::fmt::Display| Error::Config(format!("{}: {e}", path.display()));
        let text = std::fs::read_to_string(path).map_err(|e| at(&e))?;
        toml::from_str(&text).map_err(|e| at(&e))
    }

    /// The options given for `plugin`, as given; empty when there are none.
    pub fn plugin(&self, plugin: &str) -> PluginConfig {
        self.plugins.get(plugin).cloned().unwrap_or_default()
    }

    /// The options of `plugin` as the plugin receives them: every option of its manifest
    /// that has a value or a default. Fails on a key the manifest does not declare.
    pub fn plugin_config(&self, registry: &Registry, plugin: &str) -> Result<PluginConfig> {
        let mut options = self.plugin(plugin);
        let specs = registry.manifests.iter().find(|m| m.name == plugin).map(|m| m.options.as_slice());
        let specs = specs.unwrap_or_default();
        if let Some(unknown) = options.0.keys().find(|key| !specs.iter().any(|s| s.name == key.as_str())) {
            let known: Vec<_> = specs.iter().map(|s| s.name).collect();
            return Err(Error::Config(format!(
                "[plugins.{plugin}] {unknown}: plugin \"{plugin}\" has no such option; it has: {}",
                known.join(", ")
            )));
        }
        for spec in specs {
            if let (None, Some(default)) = (options.0.get(spec.name), spec.default) {
                options.set(spec.name, default);
            }
        }
        Ok(options)
    }

    /// Check the options of every plugin (`plugin_config`) and that each `[plugins.x]` names a plugin.
    pub fn check_plugins(&self, registry: &Registry) -> Result<()> {
        for name in self.plugins.keys() {
            if !registry.manifests.iter().any(|m| m.name == name) {
                return Err(Error::Config(format!("[plugins.{name}]: no plugin has this name")));
            }
            self.plugin_config(registry, name)?;
        }
        Ok(())
    }
}

/// What the storage plugin is opened with.
#[derive(Debug, Clone, Default)]
pub struct StorageConfig {
    pub options: PluginConfig,
    /// Every registered kind: declared fields, text fields, entities.
    pub kinds: Vec<KindSpec>,
    pub gds_version: String,
    pub dictionary_version: String,
    /// The dictionary as loaded, kept with the run (STO-4), and its SHA-256 in hexadecimal.
    pub dictionary_json: Vec<u8>,
    pub dictionary_hash: String,
    pub plugins: Vec<String>,
    /// The database file to open; `None`: `gds.db` in `run_dir` (STO-3).
    pub database: Option<PathBuf>,
    /// The log directory of this run. `None`: nothing may be written outside `database`.
    pub run_dir: Option<PathBuf>,
    /// Open without changing the store and without beginning a run (STO-12).
    pub read_only: bool,
}
