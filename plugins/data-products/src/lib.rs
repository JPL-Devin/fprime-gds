//! F Prime data products (design section 3.6 and CLI-5).
//!
//! - `decode`, `validate`: the decoder and validator, a library usable without a GDS.
//!   Their output is the output of the Python `fprime-dp` tool (`tests/dp/README.md`).
//! - `fgds dp decode` and `fgds dp validate`: the same command line as `fprime-dp`.
//! - Kind `fprime.data_product`: one entity per product found in the watched
//!   directory, with its header fields and validation result.
//! - Routes under `/api/v1/data-products` and the Data Products tab.

mod cli;
pub mod decode;
mod index;
pub mod json;
pub mod types;
pub mod validate;

pub use decode::{decode, DecodeOptions, Decoded, DpError};
pub use index::{Index, Options, KIND};
pub use json::Json;
pub use types::DpDictionary;

use fgds_core::{Manifest, OptionSpec, Plugin, Registry, Result, StaticFiles, UiBundle};
use std::sync::Arc;

pub const PLUGIN: &str = "data-products";

/// Option names; each is also the command-line flag.
pub const OPTION_DIR: &str = "dir";
pub const OPTION_SETTLE_MS: &str = "settle-ms";
pub const OPTION_MAX_BYTES: &str = "max-bytes";

static FILES: StaticFiles = StaticFiles(&[
    ("data-products.js", include_bytes!("../ui/data-products.js")),
    ("lit-core.min.js", include_bytes!("../ui/lit-core.min.js")),
]);

pub struct DataProductsPlugin;

impl Plugin for DataProductsPlugin {
    fn manifest(&self) -> Manifest {
        let option = |name, help, default| OptionSpec { name, help, default, secret: false };
        Manifest {
            name: PLUGIN,
            version: env!("CARGO_PKG_VERSION"),
            options: vec![
                option(OPTION_DIR, "Directory watched for data product files (*.fdp)", None),
                option(OPTION_SETTLE_MS, "Time a file must keep its size before it is indexed, in ms", Some("2000")),
                option(OPTION_MAX_BYTES, "Largest data product read, in bytes", Some("268435456")),
            ],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_option_check(Arc::new(|config, _| {
            if let Some(text) = config.get(OPTION_DIR) {
                fgds_core::check_directory(&format!("[plugins.{PLUGIN}] {OPTION_DIR}"), &text)?;
            }
            for (name, what) in [(OPTION_SETTLE_MS, "milliseconds"), (OPTION_MAX_BYTES, "bytes")] {
                if let Some(text) = config.get(name).filter(|text| text.parse::<u64>().is_err()) {
                    let message = format!("[plugins.{PLUGIN}] {name}: \"{text}\" is not a whole number of {what}");
                    return Err(fgds_core::Error::Config(message));
                }
            }
            Ok(())
        }));
        reg.add_kind(index::kind())?;
        reg.add_handler(Arc::new(|config| Ok(Box::new(index::Indexer::new(Options::from_config(config))))))?;
        reg.add_routes(index::routes())?;
        reg.add_tab(UiBundle {
            tab_id: PLUGIN,
            title: Some("Data Products"),
            order: 110,
            module: "data-products.js",
            files: &FILES,
        })?;
        reg.add_subcommand(Arc::new(cli::DpCommand))
    }
}

/// An I/O error as Python words it: `[Errno 2] No such file or directory: 'path'`.
pub(crate) fn io_error(e: &std::io::Error, path: &str) -> String {
    let text = e.to_string();
    let reason = text.split(" (os error").next().unwrap_or(&text);
    match e.raw_os_error() {
        Some(code) => format!("[Errno {code}] {reason}: '{path}'"),
        None => format!("{reason}: '{path}'"),
    }
}
