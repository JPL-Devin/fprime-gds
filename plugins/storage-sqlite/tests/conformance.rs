use fgds_core::testing::conformance::{persistence_conformance, storage_conformance_with};
use fgds_core::{Error, KindSpec, Query, Record, Storage, StorageConfig, Timestamp};
use std::sync::Arc;
use storage_sqlite::SqliteStorage;

fn make() -> Arc<dyn Storage> {
    Arc::new(SqliteStorage::default())
}

fn config(dir: &tempfile::TempDir) -> StorageConfig {
    let kinds = vec![KindSpec::new("t.sample", "")];
    StorageConfig { database: Some(dir.path().join("gds.db")), kinds, ..Default::default() }
}

fn record(seq: u64, run: u32) -> Record {
    Record {
        seq,
        run,
        ground_time: Timestamp(seq as i64),
        kind: "t.sample".into(),
        flight_time: Some(fgds_core::FlightTime { base: 2, context: 1, seconds: u32::MAX, microseconds: 999_999 }),
        id: Some(u32::MAX),
        entity: Some("e".into()),
        stored: true,
        raw: fgds_core::bytes::Bytes::from_static(&[0, 255]),
        meta: fgds_core::PacketMeta::default()
            .with("vc", fgds_core::MetaValue::U64(u64::MAX))
            .with("station", fgds_core::MetaValue::Text("x".into())),
        body: serde_json::json!({"big": "18446744073709551615", "nested": {"list": [1, 2.5, null, "é"]}}),
    }
}

#[tokio::test]
async fn sqlite_storage_conforms() {
    let dir = tempfile::tempdir().unwrap();
    storage_conformance_with(make, config(&dir)).await;
}

#[tokio::test]
async fn sqlite_storage_keeps_runs_dictionaries_and_reservations() {
    let dir = tempfile::tempdir().unwrap();
    persistence_conformance(make, config(&dir)).await;
}

#[tokio::test]
async fn the_database_is_in_the_log_directory_unless_a_file_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let storage = make();
    storage.open(&StorageConfig { run_dir: Some(dir.path().into()), ..Default::default() }).await.unwrap();
    assert!(dir.path().join("gds.db").is_file());
    let nowhere = make().open(&StorageConfig::default()).await.unwrap_err();
    assert!(matches!(nowhere, Error::Config(_)), "{nowhere}");
}

#[tokio::test]
async fn every_field_of_a_record_is_read_back_as_written() {
    let dir = tempfile::tempdir().unwrap();
    let storage = make();
    let run = storage.open(&config(&dir)).await.unwrap();
    let written = record(run.first_seq, run.run);
    storage.append(std::slice::from_ref(&written)).await.unwrap();
    assert_eq!(storage.query(&Query { limit: 10, ..Default::default() }).await.unwrap().records, [written]);
}

#[tokio::test]
async fn a_database_in_use_is_refused_and_read_only_changes_no_byte() {
    let dir = tempfile::tempdir().unwrap();
    let storage = make();
    let run = storage.open(&config(&dir)).await.unwrap();
    storage.append(&[record(run.first_seq, run.run)]).await.unwrap();
    // A second GDS on the same file (STO-10).
    let second = make().open(&config(&dir)).await.unwrap_err().to_string();
    assert!(second.ends_with("the database is in use by another GDS"), "{second}");
    storage.close().await.unwrap();
    drop(storage);

    let path = dir.path().join("gds.db");
    let before = std::fs::read(&path).unwrap();
    let read_only = make();
    read_only.open(&StorageConfig { read_only: true, ..config(&dir) }).await.unwrap();
    assert_eq!(read_only.query(&Query { limit: 10, ..Default::default() }).await.unwrap().records.len(), 1);
    assert!(read_only.reserve(5_000_000).await.is_ok(), "ignored, not written");
    read_only.close().await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // Read-only needs a database that exists and holds a run.
    let missing = StorageConfig { read_only: true, database: Some(dir.path().join("none.db")), ..Default::default() };
    assert!(make().open(&missing).await.unwrap_err().to_string().ends_with("no such database"));
    assert!(!dir.path().join("none.db").exists());
}

#[tokio::test]
async fn a_database_of_a_newer_gds_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let storage = make();
    storage.open(&config(&dir)).await.unwrap();
    storage.close().await.unwrap();
    drop(storage);
    let conn = rusqlite::Connection::open(dir.path().join("gds.db")).unwrap();
    conn.execute("UPDATE meta SET value = 99 WHERE key = 'schema_version'", []).unwrap();
    drop(conn);
    for read_only in [false, true] {
        let refused = make().open(&StorageConfig { read_only, ..config(&dir) }).await.unwrap_err().to_string();
        assert!(refused.contains("written by a newer GDS (schema 99"), "{refused}");
    }
}

#[tokio::test]
async fn a_large_history_is_paged_without_reading_it_all() {
    let dir = tempfile::tempdir().unwrap();
    let storage = make();
    let run = storage.open(&config(&dir)).await.unwrap();
    let batch: Vec<Record> = (0..20_000).map(|n| record(run.first_seq + n, run.run)).collect();
    storage.append(&batch).await.unwrap();
    let started = std::time::Instant::now();
    let mut after = None;
    let mut pages = 0;
    for _ in 0..5 {
        let page = storage.query(&Query { limit: 100, after_seq: after, ..Default::default() }).await.unwrap();
        assert_eq!(page.records.len(), 100);
        after = page.next;
        pages += 1;
    }
    assert_eq!((pages, after), (5, Some(run.first_seq + 499)));
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(storage.latest(&Query { limit: 10, ..Default::default() }).await.unwrap().records.len(), 1);
}
