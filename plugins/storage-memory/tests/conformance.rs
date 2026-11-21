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

#[tokio::test]
async fn the_oldest_records_are_dropped_beyond_max_records() {
    use fgds_core::{KindSpec, PluginConfig, Query, Record, Storage, StorageConfig, Timestamp};
    let storage = MemoryStorage::default();
    let mut options = PluginConfig::default();
    options.set("max-records", "3");
    let kinds = vec![KindSpec::new("t.data", "")];
    storage.open(&StorageConfig { options, kinds, ..Default::default() }).await.unwrap();
    let record = |seq| Record {
        seq,
        run: 1,
        ground_time: Timestamp::now(),
        kind: "t.data".into(),
        flight_time: None,
        id: None,
        entity: None,
        stored: true,
        raw: Default::default(),
        meta: Default::default(),
        body: serde_json::json!({}),
    };
    for seq in 1..=5 {
        storage.append(&[record(seq)]).await.unwrap();
    }
    let page = storage.query(&Query { limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(page.records.iter().map(|r| r.seq).collect::<Vec<_>>(), [3, 4, 5]);
    assert_eq!(storage.dropped(), 2);
    // A page ends after `limit` matches and names the next one.
    let page = storage.query(&Query { after_seq: Some(3), limit: 1, ..Default::default() }).await.unwrap();
    assert_eq!((page.records[0].seq, page.next), (4, Some(4)));
}
