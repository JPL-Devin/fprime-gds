//! The storage conformance suite (MNT-10): it fixes the meaning of every `Query` field,
//! so a second storage plugin behaves like the first. A storage plugin's tests call
//! `storage_conformance` with a function that makes a fresh, unopened storage.

use crate::{
    FieldFilter, FieldSpec, FieldType, KindSpec, Op, Query, Record, Storage, StorageConfig, TimeRange, Timestamp,
};
use serde_json::{json, Value};
use std::sync::Arc;

const FIELDS: &[FieldSpec] = &[
    FieldSpec { name: "level", pointer: "/level", ty: FieldType::Number },
    FieldSpec { name: "name", pointer: "/name", ty: FieldType::Text },
];

fn kinds() -> Vec<KindSpec> {
    vec![
        KindSpec { fields: FIELDS, text_fields: &["/name"], ..KindSpec::new("t.sample", "") },
        KindSpec { has_entities: true, ..KindSpec::new("t.state", "") },
    ]
}

fn record(seq: u64, kind: &str, id: Option<u32>, entity: Option<&str>, body: Value) -> Record {
    Record {
        seq,
        run: 1,
        ground_time: Timestamp(seq as i64 * 1000),
        kind: kind.to_string(),
        flight_time: None,
        id,
        entity: entity.map(str::to_string),
        stored: true,
        raw: bytes::Bytes::from_static(b"raw"),
        meta: Default::default(),
        body,
    }
}

fn seqs(page: &crate::Page) -> Vec<u64> {
    page.records.iter().map(|r| r.seq).collect()
}

pub async fn storage_conformance(make: impl Fn() -> Arc<dyn Storage>) {
    storage_conformance_with(make, StorageConfig::default()).await
}

/// The suite for a storage that needs more than the defaults to open (a database file).
pub async fn storage_conformance_with(make: impl Fn() -> Arc<dyn Storage>, base: StorageConfig) {
    let storage = make();
    let cfg = StorageConfig { kinds: kinds(), gds_version: "1".into(), ..base };
    let run = storage.open(&cfg).await.expect("open");
    assert!(run.first_seq >= 1, "sequence numbers start at 1 or above");
    assert_eq!(storage.runs().await.expect("runs"), vec![run.clone()]);

    let mut records = vec![
        record(1, "t.sample", Some(10), None, json!({"level": 1, "name": "Alpha"})),
        record(2, "t.sample", Some(11), None, json!({"level": 5, "name": "beta"})),
        record(3, "t.state", None, Some("a"), json!({"step": 1})),
        record(4, "t.sample", Some(10), None, json!({"level": "18446744073709551615", "name": "gamma"})),
        record(5, "t.state", None, Some("b"), json!({"step": 1})),
        record(6, "t.state", None, Some("a"), json!({"step": 2})),
    ];
    for r in &mut records {
        r.run = run.run;
    }
    // Appended in two batches; an empty batch is allowed.
    storage.append(&records[..2]).await.expect("append");
    storage.append(&[]).await.expect("append empty");
    storage.append(&records[2..]).await.expect("append");

    let all = Query { limit: 100, ..Default::default() };
    let query = |q: Query| {
        let storage = storage.clone();
        async move { storage.query(&q).await.expect("query") }
    };

    // No condition: every record of the current run, unchanged, in seq order.
    let page = query(all.clone()).await;
    assert_eq!(page.records, records);
    assert_eq!(page.next, None);

    // kinds: any of the listed kinds.
    let q = Query { kinds: vec!["t.state".into()], ..all.clone() };
    assert_eq!(seqs(&query(q).await), [3, 5, 6]);

    // run: None is the current run; another run has no records.
    assert_eq!(seqs(&query(Query { run: Some(run.run), ..all.clone() }).await), [1, 2, 3, 4, 5, 6]);
    assert_eq!(seqs(&query(Query { run: Some(run.run + 1), ..all.clone() }).await), [] as [u64; 0]);

    // after_seq and before_seq are exclusive.
    let q = Query { after_seq: Some(2), before_seq: Some(5), ..all.clone() };
    assert_eq!(seqs(&query(q).await), [3, 4]);

    // ids: any of the listed identifiers; records without one do not match.
    assert_eq!(seqs(&query(Query { ids: vec![10], ..all.clone() }).await), [1, 4]);

    // time: ground time, from inclusive, to exclusive.
    let time = TimeRange { from: Some(Timestamp(2000)), to: Some(Timestamp(4000)) };
    assert_eq!(seqs(&query(Query { time: Some(time), ..all.clone() }).await), [2, 3]);

    // filters: on declared fields; numbers compare as numbers even when carried as text.
    let filter = |field: &str, op, value: Value| Query {
        filters: vec![FieldFilter { field: field.into(), op, value }],
        ..all.clone()
    };
    assert_eq!(seqs(&query(filter("level", Op::Eq, json!(5))).await), [2]);
    assert_eq!(seqs(&query(filter("level", Op::Eq, json!("5"))).await), [2]);
    assert_eq!(seqs(&query(filter("level", Op::Ne, json!(5))).await), [1, 4]);
    assert_eq!(seqs(&query(filter("level", Op::Lt, json!(5))).await), [1]);
    assert_eq!(seqs(&query(filter("level", Op::Le, json!(5))).await), [1, 2]);
    assert_eq!(seqs(&query(filter("level", Op::Gt, json!(5))).await), [4]);
    assert_eq!(seqs(&query(filter("level", Op::Ge, json!(5))).await), [2, 4]);
    assert_eq!(seqs(&query(filter("name", Op::Eq, json!("beta"))).await), [2]);
    // Integers compare exactly, beyond what a float holds.
    assert_eq!(seqs(&query(filter("level", Op::Eq, json!("18446744073709551615"))).await), [4]);
    assert_eq!(seqs(&query(filter("level", Op::Eq, json!("18446744073709551614"))).await), [] as [u64; 0]);
    assert_eq!(seqs(&query(filter("level", Op::Gt, json!("18446744073709551614"))).await), [4]);
    assert_eq!(seqs(&query(filter("level", Op::In, json!("1, 5"))).await), [1, 2]);
    assert_eq!(seqs(&query(filter("name", Op::Prefix, json!("ga"))).await), [4]);
    // Contains is a case-sensitive substring.
    assert_eq!(seqs(&query(filter("name", Op::Contains, json!("a"))).await), [1, 2, 4]);
    assert_eq!(seqs(&query(filter("name", Op::Contains, json!("A"))).await), [1]);

    // text: case-insensitive substring of the kind's text fields.
    assert_eq!(seqs(&query(Query { text: Some("ALPHA".into()), ..all.clone() }).await), [1]);

    // Conditions combine: every one must hold.
    let q = Query { kinds: vec!["t.sample".into()], ids: vec![10], after_seq: Some(1), ..all.clone() };
    assert_eq!(seqs(&query(q).await), [4]);

    // limit: at most that many; `next` is the cursor of the following page.
    let first = query(Query { limit: 4, ..all.clone() }).await;
    assert_eq!((seqs(&first), first.next), (vec![1, 2, 3, 4], Some(4)));
    let rest = query(Query { limit: 4, after_seq: first.next, ..all.clone() }).await;
    assert_eq!((seqs(&rest), rest.next), (vec![5, 6], None));
    let exact = query(Query { limit: 6, ..all.clone() }).await;
    assert_eq!(exact.next, None, "no cursor when the page holds everything");

    // latest: per identifier, and per entity for kinds with entities; same conditions and bounds.
    let latest = storage.latest(&all).await.expect("latest");
    assert_eq!(seqs(&latest), [2, 4, 5, 6]);
    let q = Query { kinds: vec!["t.state".into()], limit: 1, ..all.clone() };
    let latest = storage.latest(&q).await.expect("latest");
    assert_eq!((seqs(&latest), latest.next), (vec![5], Some(5)));
    let q = Query { before_seq: Some(6), kinds: vec!["t.state".into()], ..all.clone() };
    assert_eq!(seqs(&storage.latest(&q).await.expect("latest")), [3, 5]);

    storage.close().await.expect("close");
}

/// What a storage that outlives the process must do (STO-4, STO-5, STO-12, STO-14).
/// `make` returns a fresh storage object each time; `base` names the same database.
pub async fn persistence_conformance(make: impl Fn() -> Arc<dyn Storage>, base: StorageConfig) {
    let config = |dictionary: &str, hash: &str| StorageConfig {
        kinds: kinds(),
        gds_version: "1".into(),
        dictionary_version: dictionary.into(),
        dictionary_json: format!("{{\"v\": \"{dictionary}\"}}").into_bytes(),
        dictionary_hash: hash.into(),
        plugins: vec!["a".into(), "b".into()],
        ..base.clone()
    };
    let in_run = |seq: u64, run: u32| Record { run, ..record(seq, "t.sample", Some(1), None, json!({"level": seq})) };

    // First run: stopped properly, the reservation given back.
    let storage = make();
    let first = storage.open(&config("d1", "h1")).await.expect("open");
    assert!(first.reserved_seq > first.first_seq, "numbers are reserved ahead");
    let start = first.first_seq;
    storage.append(&[in_run(start, first.run), in_run(start + 1, first.run)]).await.expect("append");
    storage.reserve(start + 1).await.expect("reserve");
    storage.close().await.expect("close");

    // Second run, another dictionary: continues at the next number; killed, so neither
    // the reservation is given back nor the storage closed.
    let storage = make();
    let second = storage.open(&config("d2", "h2")).await.expect("reopen");
    assert_eq!(second.run, first.run + 1);
    assert_eq!(second.first_seq, start + 2, "the next run continues at the next number");
    storage.append(&[in_run(second.first_seq, second.run)]).await.expect("append");
    // The same batch again (the pipeline could not tell whether it was stored): no duplicate.
    storage.append(&[in_run(second.first_seq, second.run)]).await.expect("append again");
    storage.reserve(second.first_seq + 500).await.expect("reserve");
    let runs = storage.runs().await.expect("runs");
    assert_eq!(runs.iter().map(|r| r.run).collect::<Vec<_>>(), [first.run, second.run]);
    assert_eq!(
        (runs[0].first_seq, &runs[0].dictionary_version, &runs[0].dictionary_hash),
        (start, &"d1".into(), &"h1".into())
    );
    assert_eq!(runs[1].plugins, ["a", "b"]);
    // Each run keeps its own dictionary.
    assert_eq!(storage.dictionary(first.run).await.expect("dictionary"), Some(b"{\"v\": \"d1\"}".to_vec()));
    assert_eq!(storage.dictionary(second.run).await.expect("dictionary"), Some(b"{\"v\": \"d2\"}".to_vec()));
    assert_eq!(storage.dictionary(second.run + 7).await.expect("dictionary"), None);
    // The current run by default, one run by number, every run when asked.
    let all = Query { limit: 100, ..Default::default() };
    let seqs_of = |q: Query| {
        let storage = storage.clone();
        async move { seqs(&storage.query(&q).await.expect("query")) }
    };
    assert_eq!(seqs_of(all.clone()).await, [start + 2]);
    assert_eq!(seqs_of(Query { run: Some(first.run), ..all.clone() }).await, [start, start + 1]);
    assert_eq!(seqs_of(Query { all_runs: true, ..all.clone() }).await, [start, start + 1, start + 2]);
    assert_eq!(seqs_of(Query { all_runs: true, after_seq: Some(start), ..all.clone() }).await, [start + 1, start + 2]);
    drop(storage);

    // Read-only: no run begins, the last run is returned, nothing can be written.
    let read_only = make();
    let last = read_only.open(&StorageConfig { read_only: true, ..config("d3", "h3") }).await.expect("read-only");
    assert_eq!((last.run, last.first_seq, last.dictionary_hash.as_str()), (second.run, second.first_seq, "h2"));
    assert_eq!(read_only.runs().await.expect("runs").len(), 2);
    assert!(read_only.append(&[in_run(start + 900, last.run)]).await.is_err(), "read-only refuses to store");
    assert_eq!(seqs(&read_only.query(&all).await.expect("query")), [start + 2]);
    assert_eq!(read_only.dictionary(first.run).await.expect("dictionary"), Some(b"{\"v\": \"d1\"}".to_vec()));
    read_only.close().await.expect("close");

    // Third run after the kill: starts after everything the second run had reserved.
    let storage = make();
    let third = storage.open(&config("d2", "h2")).await.expect("reopen after a kill");
    assert_eq!(third.run, second.run + 1);
    assert_eq!(third.first_seq, second.first_seq + 501, "no number of the killed run is used again");
    storage.close().await.expect("close");
}
