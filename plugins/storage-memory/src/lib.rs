//! Storage plugin `memory`: one run kept in memory, lost at exit. For tests and for runs
//! that need no history: at most `max-records` records are kept, the oldest dropped.

use fgds_core::testing::MemoryStorage;
use fgds_core::{Manifest, OptionSpec, Plugin, Registry, Result};
use std::sync::Arc;

pub struct MemoryStoragePlugin;

impl Plugin for MemoryStoragePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "storage-memory",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![OptionSpec {
                name: "max-records",
                help: "Keep at most this many records, dropping the oldest (0: keep all)",
                default: Some("1000000"),
                secret: false,
            }],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_option_check(Arc::new(|cfg, _| match cfg.get("max-records") {
            Some(text) if text.parse::<usize>().is_err() => Err(fgds_core::Error::Config(format!(
                "storage-memory max-records: \"{text}\" is not a number of records"
            ))),
            _ => Ok(()),
        }));
        reg.add_storage("memory", Arc::new(MemoryStorage::default()))
    }
}
