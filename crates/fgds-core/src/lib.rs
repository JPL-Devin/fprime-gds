//! The F Prime GDS core: what plugins plug into.
//!
//! Records and the pipeline (number, store, broadcast), the plugin registry and its
//! conflict checks, the packet router, the uplink service, status, configuration,
//! supervised plugin tasks, the extension-point traits, and test doubles for plugins.

mod config;
mod error;
mod http;
mod link;
mod pipeline;
mod plugin;
mod record;
mod registry;
mod router;
mod runtime;
mod status;
mod supervisor;
pub mod testing;
mod uplink;

pub use config::{Config, PluginConfig, StorageConfig};
pub use error::{Error, Result};
pub use http::ApiError;
pub use link::{Link, LinkError};
pub use pipeline::{PipelineOptions, RecordStream, Records, StreamEnd, StreamItem};
pub use plugin::*;
pub use record::*;
pub use registry::Registry;
pub use router::{keep_decoded, Router};
pub use runtime::{framing_layers, named, open_read_only, stored_dictionary, Context, Core, ShutdownToken};
pub use status::{Status, StatusHandle};
pub use supervisor::{report_fault, spawn_supervised};
pub use uplink::{SendError, Sent, Uplink};

// Third-party types that appear in the extension-point interface (CUS-11).
pub use async_trait::async_trait;
pub use fgds_dictionary as dictionary;
pub use fgds_dictionary::Dictionary;
pub use {axum, bytes, clap, serde_json};

/// Version of the extension-point interface and of the stock GDS.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
