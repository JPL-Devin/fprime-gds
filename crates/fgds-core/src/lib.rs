//! The F Prime GDS core: what plugins plug into.
//!
//! Records and the pipeline (number, store, broadcast), the plugin registry and its
//! conflict checks, the packet router, the uplink service, status, configuration,
//! supervised plugin tasks, the extension-point traits, and test doubles for plugins.

pub mod argparse;
mod config;
mod error;
pub mod gds_options;
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
mod text;
mod uplink;

pub use config::{Config, PluginConfig, StorageConfig};
pub use error::{Error, Result};
pub use http::ApiError;
pub use link::{max_packet, Link, LinkError};
pub use pipeline::{PipelineOptions, RecordStream, Records, StreamEnd, StreamItem};
pub use plugin::*;
pub use record::*;
pub use registry::Registry;
pub use router::{keep_decoded, Router};
pub use runtime::{
    framing_layers, named, open_read_only, preflight, stored_dictionary, Context, Core, Preflight, ShutdownToken,
};
pub use status::{Status, StatusHandle};
pub use supervisor::{report_fault, spawn_supervised};
pub use text::{visible, visible_lines};
pub use uplink::{SendError, Sent, Uplink};

// Third-party types that appear in the extension-point interface (CUS-11).
pub use async_trait::async_trait;
pub use fgds_dictionary as dictionary;
pub use fgds_dictionary::Dictionary;
pub use {axum, bytes, clap, serde_json};

/// Version of the extension-point interface and of the stock GDS.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Completes on Ctrl-C. Never completes when the process was started with SIGINT ignored (a
/// background job of a shell script): installing a handler would undo that, and `fprime-cli`
/// keeps running there. Read from `/proc/self/status`, so on Linux only.
pub async fn interrupt() {
    let ignored = std::fs::read_to_string("/proc/self/status").ok().and_then(|status| {
        let mask = status.lines().find_map(|line| line.strip_prefix("SigIgn:"))?;
        u64::from_str_radix(mask.trim(), 16).ok()
    });
    if ignored.is_some_and(|mask| mask & 0b10 != 0) {
        return std::future::pending().await;
    }
    let _ = tokio::signal::ctrl_c().await;
}
