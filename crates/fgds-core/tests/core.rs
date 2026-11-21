//! Core behaviour through the test doubles: pipeline order, replay then live, routing,
//! uplink outcomes, link control and fault containment.

use fgds_core::testing::{empty_dictionary, FakeGds, LengthFraming};
use fgds_core::*;
use serde_json::json;
use std::sync::Arc;

/// Decodes packets of one type into `t.packet` records. Type 9 fails, type 8 panics.
struct TestDecoder(PacketMatch);

impl Decoder for TestDecoder {
    fn matches(&self) -> PacketMatch {
        self.0.clone()
    }
    fn decode(&self, packet: &[u8], _: &PacketMeta, _: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        match packet[1] {
            9 => Err(DecodeError("bad packet".into())),
            8 => panic!("decoder bug"),
            _ => Ok(vec![NewRecord {
                raw: bytes::Bytes::copy_from_slice(packet),
                ..NewRecord::new("t.packet", json!({"len": packet.len()}))
            }]),
        }
    }
}

struct TestEncoder;

impl Encoder for TestEncoder {
    fn request_kind(&self) -> &'static str {
        "test"
    }
    fn record_kind(&self) -> &'static str {
        "t.sent"
    }
    fn encode(&self, request: &serde_json::Value, _: &Dictionary) -> Result<Encoded, ValidationError> {
        match request["byte"].as_u64() {
            Some(b) => Ok(Encoded {
                packet: vec![b as u8].into(),
                id: Some(7),
                body: json!({"byte": b}),
                meta: Default::default(),
            }),
            None => Err(ValidationError("byte is missing".into())),
        }
    }
}

/// Panics on the first record it receives.
struct PanicHandler;

#[async_trait]
impl Handler for PanicHandler {
    fn subscription(&self) -> Filter {
        Filter::kinds(&["t.packet"])
    }
    async fn run(self: Box<Self>, _: Context, mut records: RecordStream) -> Result<()> {
        records.next().await;
        panic!("handler bug");
    }
}

fn typed(packet_type: u32) -> PacketMatch {
    PacketMatch { packet_type: Some(PacketType(packet_type)), meta: vec![] }
}

struct TestPlugin;

impl Plugin for TestPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "test", version: "1", ..Default::default() }
    }
    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec::new("t.packet", ""))?;
        reg.add_kind(KindSpec { has_entities: true, ..KindSpec::new("t.sent", "") })?;
        reg.add_kind(KindSpec { transient: true, ..KindSpec::new("t.transient", "") })?;
        for packet_type in [1, 8, 9] {
            reg.add_decoder(Arc::new(move |_| Ok(Box::new(TestDecoder(typed(packet_type))))))?;
        }
        reg.add_encoder("test", Arc::new(|_| Ok(Box::new(TestEncoder))))
    }
}

struct FaultyPlugin;

impl Plugin for FaultyPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "faulty", version: "1", requires_kinds: vec!["t.packet"], ..Default::default() }
    }
    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_handler(Arc::new(|_| Ok(Box::new(PanicHandler))))?;
        Ok(())
    }
}

async fn gds() -> FakeGds {
    FakeGds::start(&[&TestPlugin], empty_dictionary()).await.unwrap()
}

#[tokio::test]
async fn records_are_numbered_stored_then_broadcast() {
    let gds = gds().await;
    let records = &gds.core.records;
    let mut live = records.subscribe(Filter::default(), None);
    for n in 0..5 {
        let seq = records.publish(NewRecord::new("t.packet", json!({"n": n}))).await.unwrap();
        assert_eq!(seq, n + 1);
    }
    for n in 1..=5 {
        let record = live.next().await.unwrap();
        assert_eq!((record.seq, record.run, record.stored), (n, 1, true));
        // Broadcast follows storage: a live record is already in the store.
        assert!(records.cursor() >= n);
        let stored = records.query(Query { after_seq: Some(n - 1), limit: 1, ..Default::default() }).await.unwrap();
        assert_eq!(stored.records[0], record);
    }
    assert!(records.publish(NewRecord::new("t.unregistered", json!({}))).await.is_err());
}

#[tokio::test]
async fn transient_records_are_live_only() {
    let gds = gds().await;
    let mut live = gds.core.records.subscribe(Filter::kinds(&["t.transient"]), None);
    gds.core.records.publish(NewRecord::new("t.transient", json!({}))).await.unwrap();
    // `stored` is false only for a record that storage lost, not for a transient one.
    assert!(live.next().await.unwrap().stored);
    assert!(gds.records(&["t.transient"]).await.is_empty());
}

#[tokio::test]
async fn subscription_replays_then_continues_live_without_gap_or_repeat() {
    let gds = gds().await;
    let records = gds.core.records.clone();
    for n in 0..50 {
        records.publish(NewRecord::new("t.packet", json!({"n": n}))).await.unwrap();
    }
    // Publish while the subscriber is replaying.
    let publisher = tokio::spawn({
        let records = records.clone();
        async move {
            for n in 50..2500 {
                let kind = if n % 2 == 0 { "t.packet" } else { "t.sent" };
                records.publish(NewRecord::new(kind, json!({"n": n}))).await.unwrap();
            }
        }
    });
    let mut stream = records.subscribe(Filter::kinds(&["t.packet"]), Some(10));
    let mut seen = Vec::new();
    while seen.last() != Some(&2499) {
        let record = stream.next_strict().await.unwrap();
        assert_eq!(record.kind, "t.packet");
        seen.push(record.seq);
    }
    publisher.await.unwrap();
    let expected: Vec<u64> = (11..=50).chain((51..=2499).step_by(2)).collect();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn slow_subscriber_is_told_its_last_seq_or_resumes_from_storage() {
    let gds = gds().await;
    let records = gds.core.records.clone();
    let mut strict = records.subscribe(Filter::default(), None);
    let mut resuming = records.subscribe(Filter::default(), None);
    records.publish(NewRecord::new("t.packet", json!({}))).await.unwrap();
    assert_eq!(strict.next_strict().await.unwrap().seq, 1);
    assert_eq!(resuming.next().await.unwrap().seq, 1);
    // More than a subscriber queue holds, with nobody reading.
    for _ in 0..20_000 {
        records.publish(NewRecord::new("t.packet", json!({}))).await.unwrap();
    }
    records.flush().await;
    assert_eq!(strict.next_strict().await, Err(StreamEnd::Lagged { last_seq: 1 }));
    for seq in 2..=20_001 {
        assert_eq!(resuming.next().await.unwrap().seq, seq);
    }
}

#[tokio::test]
async fn packets_are_routed_and_bad_ones_recorded_without_stopping_the_link() {
    let gds = gds().await;
    gds.receive(&[0, 1, 0xAA]).await; // decoded
    gds.receive(&[0, 9]).await; // decoder error
    gds.receive(&[0, 8]).await; // decoder panic
    gds.receive(&[0, 5]).await; // no decoder
    gds.link.push(&[0, 0]); // framing error: input dropped, layer rebuilt
    gds.receive(&[0, 1]).await; // still decoded afterwards

    let all = gds.records(&[]).await;
    let kinds: Vec<&str> = all.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["t.packet", "gds.decode_error", "gds.decode_error", "gds.unknown_packet", "raw.discarded", "t.packet"]
    );
    assert_eq!(all[0].raw.as_ref(), [0, 1, 0xAA]);
    assert_eq!(all[1].body["reason"], "bad packet");
    assert_eq!(all[2].body["reason"], "decoder panicked: decoder bug");
    assert_eq!(all[3].body["packet_type"], 5);
    assert_eq!(all[4].body["reason"], "framing layer failed: zero-length frame");
    assert_eq!(gds.core.status.snapshot()["counters"]["packets_received"], 5);
}

#[tokio::test]
async fn a_frame_split_across_reads_is_reassembled() {
    let gds = gds().await;
    let frame = LengthFraming::frame(&[0, 1, 2, 3]);
    gds.link.push(&frame[..3]);
    gds.link.push(&frame[3..]);
    for _ in 0..1000 {
        if !gds.records(&["t.packet"]).await.is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("packet not received");
}

#[tokio::test]
async fn uplink_validates_sends_and_records_every_outcome() {
    let gds = gds().await;
    let uplink = &gds.core.uplink;

    // Dry run: encoded, not sent, not recorded.
    assert_eq!(uplink.dry_run("test", &json!({"byte": 5})).unwrap().packet.as_ref(), [5]);
    assert!(gds.link.sent().is_empty());

    let sent = uplink.send("test", &json!({"byte": 5}), "tester").await.unwrap();
    assert_eq!(gds.link.sent(), [LengthFraming::frame(&[5])]);

    let invalid = uplink.send("test", &json!({}), "tester").await;
    assert_eq!(invalid, Err(SendError::Invalid("byte is missing".into())));
    assert_eq!(uplink.send("nope", &json!({}), "tester").await, Err(SendError::UnknownKind("nope".into())));

    // Link disabled: rejected, never queued (LNK-11, LNK-22).
    gds.core.link.set_enabled(false);
    gds.wait_for_link("disabled").await;
    assert_eq!(uplink.send("test", &json!({"byte": 6}), "tester").await, Err(SendError::LinkDown));
    gds.core.link.set_enabled(true);
    gds.wait_for_link("connected").await;
    uplink.send("test", &json!({"byte": 7}), "tester").await.unwrap();
    assert_eq!(gds.link.sent().len(), 2, "the rejected command was not sent later");

    let records = gds.records(&["t.sent"]).await;
    let outcomes: Vec<_> = records.iter().map(|r| r.body["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["sent", "rejected", "rejected", "sent"]);
    assert_eq!(records[0].seq, sent.seq);
    assert_eq!(records[0].entity.as_deref(), Some(sent.entity.as_str()));
    assert_eq!((records[0].id, records[0].body["client"].as_str()), (Some(7), Some("tester")));
    assert_eq!(records[0].body["link_bytes"], "AAEF"); // base64 of 00 01 05
    assert_eq!(records[2].body["reason"], "link down");
}

#[tokio::test]
async fn link_reconnects_after_the_peer_closes() {
    let gds = gds().await;
    gds.link.close();
    gds.wait_for_link("disconnected").await;
    gds.wait_for_link("connected").await;
    gds.receive(&[0, 1]).await;
    assert_eq!(gds.records(&["t.packet"]).await.len(), 1);
}

#[tokio::test]
async fn a_panicking_handler_is_contained_and_reported() {
    let gds = FakeGds::start(&[&TestPlugin, &FaultyPlugin], empty_dictionary()).await.unwrap();
    gds.receive(&[0, 1]).await;
    let mut faults = gds.core.records.subscribe(Filter::kinds(&[KIND_PLUGIN_FAULT]), Some(0));
    let fault = faults.next().await.unwrap();
    assert_eq!(fault.body, json!({"plugin": "faulty", "message": "handler bug"}));
    let status = gds.core.status.snapshot();
    assert_eq!(status["plugins"]["faulty"]["state"], "faulted");
    assert_eq!(status["plugins"]["faulty"]["fault"], "handler bug");
    // The rest keeps working.
    gds.receive(&[0, 1]).await;
    assert_eq!(gds.records(&["t.packet"]).await.len(), 2);
}

#[tokio::test]
async fn invalid_queries_are_rejected() {
    let gds = gds().await;
    let filter = FieldFilter { field: "nope".into(), op: Op::Eq, value: json!(1) };
    let query = Query { filters: vec![filter], ..Default::default() };
    assert!(matches!(gds.core.records.query(query).await, Err(Error::Invalid(_))));
}

/// Memory storage whose `append` fails while the flag is set.
struct FlakyStorage(testing::MemoryStorage, std::sync::atomic::AtomicBool);

#[async_trait]
impl Storage for FlakyStorage {
    async fn open(&self, cfg: &StorageConfig) -> Result<RunInfo> {
        self.0.open(cfg).await
    }
    async fn append(&self, batch: &[Record]) -> Result<()> {
        match self.1.load(std::sync::atomic::Ordering::SeqCst) {
            true => Err(Error::other("disk full")),
            false => self.0.append(batch).await,
        }
    }
    async fn query(&self, q: &Query) -> Result<Page> {
        self.0.query(q).await
    }
    async fn latest(&self, q: &Query) -> Result<Page> {
        self.0.latest(q).await
    }
    async fn runs(&self) -> Result<Vec<RunInfo>> {
        self.0.runs().await
    }
    async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

#[tokio::test]
async fn storage_failure_is_broadcast_unstored_and_reported_on_recovery() {
    let kinds = [
        KindSpec::new("t.data", "test"),
        KindSpec::new(KIND_STORAGE_GAP, "gap"),
        KindSpec::new(KIND_STORAGE_RECOVERED, "recovered"),
    ];
    let storage = Arc::new(FlakyStorage(Default::default(), true.into()));
    let run = storage.open(&StorageConfig { kinds: kinds.to_vec(), ..Default::default() }).await.unwrap();
    let status = Status::default();
    let records = Records::start(storage.clone(), &run, &kinds, status.clone());
    let mut live = records.subscribe(Filter::default(), None);

    records.publish(NewRecord::new("t.data", json!(1))).await.unwrap();
    let lost = live.next().await.unwrap();
    assert_eq!((lost.seq, lost.stored), (1, false));
    assert!(status.snapshot()["storage"]["state"].as_str().unwrap().starts_with("failed"));

    storage.1.store(false, std::sync::atomic::Ordering::SeqCst);
    records.publish(NewRecord::new("t.data", json!(2))).await.unwrap();
    let after: Vec<_> = [live.next().await, live.next().await, live.next().await].into_iter().flatten().collect();
    assert_eq!(
        after.iter().map(|r| (r.kind.as_str(), r.stored)).collect::<Vec<_>>(),
        [("t.data", true), (KIND_STORAGE_GAP, true), (KIND_STORAGE_RECOVERED, true)]
    );
    assert_eq!(after[1].body, json!({"first_seq": "1", "last_seq": "1"}));
    assert_eq!(status.snapshot()["storage"]["state"], "ok");
    // A replay from the start crosses the gap and is told about it.
    let replay = records.query(Query::default()).await.unwrap();
    assert_eq!(replay.records.iter().map(|r| r.seq).collect::<Vec<_>>(), [2, 3, 4]);
}
