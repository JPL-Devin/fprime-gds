use crate::{Error, KindSpec, Result};
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
            framing: "fprime".into(),
            framing_layers: Vec::new(),
            storage: "memory".into(),
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

    /// The options of `plugin`; empty when it has none.
    pub fn plugin(&self, plugin: &str) -> PluginConfig {
        self.plugins.get(plugin).cloned().unwrap_or_default()
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
    pub plugins: Vec<String>,
}
