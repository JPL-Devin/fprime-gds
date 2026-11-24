//! The storage failure path of the pipeline (STO-9), on a storage that fails or hangs.

use fgds_core::testing::{Fault, FaultyStorage};
use fgds_core::*;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

async fn start() -> (Arc<FaultyStorage>, Records, Status) {
    let registry = Registry::new();
    let mut kinds = registry.kinds.clone();
    kinds.push(KindSpec::new("t.kind", ""));
    let storage = Arc::new(FaultyStorage::default());
    let run = storage.open(&StorageConfig { kinds: kinds.clone(), ..Default::default() }).await.unwrap();
    let status = Status::default();
    let options = PipelineOptions { commit_timeout: Duration::from_millis(100), read_only: false };
    let records = Records::start_with(storage.clone(), &run, &kinds, status.clone(), options);
    (storage, records, status)
}

async fn publish(records: &Records, n: u64) -> u64 {
    records.publish(NewRecord::new("t.kind", json!({"n": n}))).await.unwrap()
}

async fn fails_then_recovers(fault: Fault) {
    let (storage, records, status) = start().await;
    let mut live = records.subscribe(Filter::default(), None);
    assert_eq!(publish(&records, 1).await, 1);
    assert!(live.next().await.unwrap().stored);

    storage.set(fault);
    assert_eq!(publish(&records, 2).await, 2);
    // Still delivered live, marked as not stored, within the commit time-out.
    let lost = tokio::time::timeout(Duration::from_secs(2), live.next()).await.unwrap().unwrap();
    assert_eq!((lost.seq, lost.stored), (2, false));
    assert!(status.snapshot()["storage"]["state"].as_str().unwrap().starts_with("failed"));
    assert_eq!(publish(&records, 3).await, 3);
    assert!(!tokio::time::timeout(Duration::from_secs(2), live.next()).await.unwrap().unwrap().stored);
    // Publishing does not wait while storage is failed, and an uplink record never waits.
    assert_eq!(records.publish_uplink(NewRecord::new("t.kind", json!({}))).unwrap(), 4);
    assert!(!tokio::time::timeout(Duration::from_secs(2), live.next()).await.unwrap().unwrap().stored);

    storage.set(Fault::None);
    assert_eq!(publish(&records, 5).await, 5);
    let stored = live.next().await.unwrap();
    assert_eq!((stored.seq, stored.stored), (5, true));
    // The gap and the recovery are records, with the numbers of what was lost.
    let gap = live.next().await.unwrap();
    assert_eq!(gap.kind, KIND_STORAGE_GAP);
    assert_eq!(gap.body, json!({"first_seq": "2", "last_seq": "4"}));
    assert_eq!(live.next().await.unwrap().kind, KIND_STORAGE_RECOVERED);
    records.flush().await;
    assert_eq!(status.snapshot()["storage"]["state"], "ok");
    // Sequence numbers are not reused and the lost records are not in storage.
    let page = records.query(Query::default()).await.unwrap();
    let seqs: Vec<u64> = page.records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [1, 5, 6, 7]);
}

#[tokio::test]
async fn a_failing_storage_gives_unstored_records_then_a_gap_and_a_recovery() {
    fails_then_recovers(Fault::Fail).await;
}

#[tokio::test]
async fn a_hanging_storage_is_cut_by_the_commit_time_out() {
    fails_then_recovers(Fault::Hang).await;
}

#[tokio::test]
async fn a_subscriber_misses_no_record_when_uplink_records_join_a_backlog() {
    let (storage, records, _) = start().await;
    let mut live = records.subscribe(Filter::default(), None);
    // Hold the commit task on a first batch, then queue more than one batch of telemetry with
    // uplink records among it.
    storage.set(Fault::Hang);
    publish(&records, 0).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    storage.set(Fault::None);
    let mut last = 0;
    for n in 1..=1400 {
        last = match n % 100 {
            0 => records.publish_uplink(NewRecord::new("t.kind", json!({"uplink": true}))).unwrap(),
            _ => publish(&records, n).await,
        };
    }
    // Every number once, in order: none is skipped because a later one was delivered first.
    for expected in 1..=last {
        assert_eq!(live.next().await.unwrap().seq, expected);
    }
    records.flush().await;
    let page = records.query(Query { limit: 2000, ..Default::default() }).await.unwrap();
    // The record the storage hung on is the only one not stored.
    assert!(page.records.iter().map(|r| r.seq).eq(2..=last));
}
