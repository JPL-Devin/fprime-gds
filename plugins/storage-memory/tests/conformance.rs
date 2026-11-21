use fgds_core::testing::{conformance::storage_conformance, MemoryStorage};
use fgds_core::Registry;
use std::sync::Arc;

#[tokio::test]
async fn memory_storage_conforms() {
    storage_conformance(|| Arc::new(MemoryStorage::default())).await;
}

#[test]
fn plugin_registers_the_memory_storage() {
    let mut registry = Registry::new();
    registry.register(&storage_memory::MemoryStoragePlugin).unwrap();
    assert!(registry.storages.contains_key("memory"));
}
