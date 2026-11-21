//! Storage plugin `memory`: one run kept in memory, lost at exit. The default of the
//! stock binary until a persistent storage plugin exists, and the storage of tests.

use fgds_core::testing::MemoryStorage;
use fgds_core::{Manifest, Plugin, Registry, Result};
use std::sync::Arc;

pub struct MemoryStoragePlugin;

impl Plugin for MemoryStoragePlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "storage-memory", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_storage("memory", Arc::new(MemoryStorage::default()))
    }
}
