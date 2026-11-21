//! Composition of a GDS binary (section 4.3 of the design):
//!
//! ```no_run
//! fn main() -> std::process::ExitCode {
//!     fgds::Gds::builder().with_defaults().without("channels").run()
//! }
//! ```
//!
//! A project binary lists its plugins here; nothing is loaded at run time (CUS-1).

mod cli;

use fgds_core::{Decoder, Error, Plugin, Registry, Result};
use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;

pub use fgds_core as core;

/// The plugins of the stock binary.
pub fn default_plugins() -> Vec<Box<dyn Plugin>> {
    // One line per plugin.
    vec![
        Box::new(storage_memory::MemoryStoragePlugin),
        Box::new(comm_tcp::TcpPlugin),
        Box::new(framing_fprime::FprimeFramingPlugin),
        Box::new(framing_ccsds::CcsdsFramingPlugin),
        Box::new(events::EventsPlugin),
        Box::new(channels::ChannelsPlugin),
        Box::new(commands::CommandsPlugin),
        Box::new(online::OnlinePlugin),
        Box::new(files::FilesPlugin::default()),
        Box::new(sequences::SequencesPlugin),
        Box::new(params::ParamsPlugin),
        Box::new(dictionary_tab::DictionaryTabPlugin),
        Box::new(logs::LogsPlugin),
        Box::new(advanced::AdvancedPlugin),
        Box::new(data_products::DataProductsPlugin),
    ]
}

pub struct Gds;

impl Gds {
    pub fn builder() -> GdsBuilder {
        GdsBuilder { plugins: Vec::new(), overrides: Vec::new(), problems: Vec::new() }
    }
}

pub struct GdsBuilder {
    plugins: Vec<Box<dyn Plugin>>,
    overrides: Vec<Arc<dyn Decoder>>,
    /// Mistakes in the composition, reported when the binary starts.
    problems: Vec<String>,
}

impl GdsBuilder {
    /// Add all default plugins.
    pub fn with_defaults(mut self) -> Self {
        self.plugins.extend(default_plugins());
        self
    }

    /// Add one plugin.
    pub fn with(mut self, plugin: impl Plugin) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Omit the plugin called `name`.
    pub fn without(mut self, name: &str) -> Self {
        match self.position(name) {
            Some(index) => drop(self.plugins.remove(index)),
            None => self.problems.push(format!("without(\"{name}\"): no such plugin")),
        }
        self
    }

    /// Put `plugin` in the place of the plugin called `name`.
    pub fn replace(mut self, name: &str, plugin: impl Plugin) -> Self {
        match self.position(name) {
            Some(index) => self.plugins[index] = Box::new(plugin),
            None => self.problems.push(format!("replace(\"{name}\"): no such plugin")),
        }
        self
    }

    /// Use `decoder` instead of the registered decoder with the same match rule.
    pub fn override_decoder(mut self, decoder: impl Decoder + 'static) -> Self {
        self.overrides.push(Arc::new(decoder));
        self
    }

    /// Run the command line of this process.
    pub fn run(self) -> ExitCode {
        self.run_from(std::env::args_os())
    }

    /// Run the given command line; `args[0]` is the program name.
    pub fn run_from<I, T>(self, args: I) -> ExitCode
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        match cli::run(&self, args.into_iter().map(Into::into).collect()) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.manifest().name == name)
    }

    /// Register every plugin not in `disabled`, note the overrides and check for
    /// conflicts (CUS-10). Decoders are built, and their match rules checked, when the
    /// configuration is known (`Registry::build_decoders`). No I/O happens here.
    fn registry(&self, disabled: &[String]) -> Result<Registry> {
        if let Some(problem) = self.problems.first() {
            return Err(Error::Config(problem.clone()));
        }
        if let Some(unknown) = disabled.iter().find(|name| self.position(name).is_none()) {
            return Err(Error::Config(format!("disabled_plugins: no plugin is called \"{unknown}\"")));
        }
        let mut registry = Registry::new();
        for plugin in &self.plugins {
            if !disabled.iter().any(|name| name == plugin.manifest().name) {
                registry.register(plugin.as_ref())?;
            }
        }
        for decoder in &self.overrides {
            registry.override_decoder(decoder.clone());
        }
        registry.check()?;
        Ok(registry)
    }
}
