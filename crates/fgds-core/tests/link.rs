//! The link under failure: writes that fail or never finish, connection errors, adapter
//! faults, read sizes, and stopping the core.

use fgds_core::testing::{empty_dictionary, FakeGds, LengthFraming, Writes};
use fgds_core::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Passthrough;

impl Decoder for Passthrough {
    fn matches(&self) -> PacketMatch {
        PacketMatch::default()
    }
    fn decode(&self, packet: &[u8], _: &PacketMeta, _: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        Ok(vec![NewRecord::new("t.packet", json!({"len": packet.len()}))])
    }
}

/// Never returns and ignores the shutdown signal.
struct StuckHandler;

#[async_trait]
impl Handler for StuckHandler {
    fn subscription(&self) -> Filter {
        Filter::kinds(&["t.packet"])
    }
    async fn run(self: Box<Self>, _: Context, _records: RecordStream) -> Result<()> {
        std::future::pending().await
    }
}

/// Publishes one record when told to stop.
struct LastWordHandler;

#[async_trait]
impl Handler for LastWordHandler {
    fn subscription(&self) -> Filter {
        Filter::kinds(&["t.packet"])
    }
    async fn run(self: Box<Self>, mut ctx: Context, _records: RecordStream) -> Result<()> {
        ctx.shutdown.cancelled().await;
        ctx.records.publish(NewRecord::new("t.packet", json!({"last": true}))).await?;
        Ok(())
    }
}

struct TestPlugin {
    stuck: bool,
    /// How many times the adapter factory of `broken` was called.
    built: Arc<AtomicUsize>,
}

impl TestPlugin {
    fn new(stuck: bool) -> TestPlugin {
        TestPlugin { stuck, built: Default::default() }
    }
}

impl Plugin for TestPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "test", version: "1", ..Default::default() }
    }
    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec::new("t.packet", ""))?;
        reg.add_decoder(Arc::new(|_| Ok(Box::new(Passthrough))))?;
        reg.add_handler(Arc::new(|_| Ok(Box::new(LastWordHandler))))?;
        if self.stuck {
            reg.add_handler(Arc::new(|_| Ok(Box::new(StuckHandler))))?;
        }
        let built = self.built.clone();
        reg.add_adapter(
            "broken",
            // Built at startup, panics, and cannot be built again.
            Arc::new(move |_| match built.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(Box::new(PanickingAdapter)),
                _ => Err(Error::other("no such device")),
            }),
        )?;
        reg.add_framing_profile("none", &[])?;
        reg.add_adapter("panics", Arc::new(|_| Ok(Box::new(PanickingAdapter))))
    }
}

struct PanickingAdapter;

#[async_trait]
impl Adapter for PanickingAdapter {
    async fn connect(&mut self) -> Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo)> {
        panic!("adapter bug")
    }
}

async fn gds() -> FakeGds {
    FakeGds::start(&[&TestPlugin::new(false)], empty_dictionary()).await.unwrap()
}

async fn status_records(gds: &FakeGds) -> Vec<serde_json::Value> {
    gds.records(&[KIND_STATUS]).await.into_iter().map(|r| r.body).collect()
}

#[tokio::test]
async fn a_write_that_never_finishes_fails_the_send_and_drops_the_connection() {
    let gds = gds().await;
    gds.core.link.set_write_timeout(Duration::from_millis(50));
    gds.link.set_writes(Writes::Stall);
    let sent = gds.core.uplink.send_packet(&[1, 2], &PacketMeta::default(), "tester", true).await;
    assert_eq!(sent, Err(SendError::Failed("write timed out".into())));
    // Half a frame may be on the link: the writer is gone and the link reconnects.
    let status = status_records(&gds).await;
    assert_eq!(status[1], json!({"link": "disconnected", "peer": null, "error": "write timed out"}));
    gds.link.set_writes(Writes::Accept);
    gds.wait_for_link("connected").await;
    gds.core.uplink.send_packet(&[3], &PacketMeta::default(), "tester", true).await.unwrap();
    assert_eq!(gds.link.sent(), [LengthFraming::frame(&[3])]);

    let records = gds.records(&[KIND_RAW_SENT]).await;
    assert_eq!(
        records[0].body,
        json!({"length": 2, "client": "tester", "outcome": "failed", "reason": "write timed out"})
    );
    assert_eq!(records[1].body["outcome"], "sent");
}

#[tokio::test]
async fn a_write_error_is_a_failed_send_and_a_status_record() {
    let gds = gds().await;
    gds.link.set_writes(Writes::Fail);
    let sent = gds.core.uplink.send_packet(&[1], &PacketMeta::default(), "tester", false).await;
    assert_eq!(sent, Err(SendError::Failed("write failed: broken pipe".into())));
    gds.link.set_writes(Writes::Accept);
    gds.wait_for_link("connected").await;
    let status = status_records(&gds).await;
    assert_eq!(status[0], json!({"link": "connected", "peer": "scripted", "error": null}));
    assert_eq!(status[1]["error"], "write failed: broken pipe");
    assert_eq!(status[2]["link"], "connected");
    assert_eq!(gds.records(&[KIND_RAW_SENT]).await[0].body["outcome"], "failed");
}

#[tokio::test]
async fn a_connection_error_is_in_the_status_and_recorded_once() {
    let gds = gds().await;
    gds.link.fail_connect(Some("address in use"));
    gds.link.close();
    for _ in 0..3000 {
        if gds.core.status.snapshot()["link"]["error"] == "address in use" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        gds.core.status.snapshot()["link"],
        json!({"state": "disconnected", "peer": null, "error": "address in use"})
    );
    // A second failed attempt with the same error adds no record.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let errors: Vec<_> = status_records(&gds).await.into_iter().filter(|s| s["error"] == "address in use").collect();
    assert_eq!(errors.len(), 1);
    gds.link.fail_connect(None);
    gds.wait_for_link("connected").await;
    assert_eq!(gds.core.status.snapshot()["link"]["error"], json!(null));
}

#[tokio::test]
async fn one_read_takes_what_the_link_has() {
    let gds = gds().await;
    gds.receive(&[0, 1]).await;
    let reads = || gds.core.status.snapshot()["counters"]["link_reads"].as_u64().unwrap();
    let before = reads();
    // The buffer was emptied by the first read: the next must still take 20,000 bytes at once.
    gds.receive(&vec![7; 20_000]).await;
    assert_eq!(reads() - before, 1);
    assert_eq!(gds.records(&["t.packet"]).await[1].body["len"], 20_000);
}

#[tokio::test]
async fn an_adapter_factory_that_fails_is_reported_once_under_its_plugin_and_retried() {
    let plugin = TestPlugin::new(false);
    let config = Config { comm: "broken".into(), framing: "length".into(), ..FakeGds::config() };
    let gds = FakeGds::start_with(&[&plugin], empty_dictionary(), config).await.unwrap();
    // Built again after 1 s and after 2 s more: two failed attempts, one report.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(plugin.built.load(Ordering::SeqCst), 3);
    let faults: Vec<_> = gds.records(&[KIND_PLUGIN_FAULT]).await.into_iter().map(|r| r.body).collect();
    assert_eq!(
        faults,
        [json!({"plugin": "test", "message": "adapter bug"}), json!({"plugin": "test", "message": "no such device"}),]
    );
    assert_eq!(gds.core.status.snapshot()["link"]["error"], "no such device");
}

#[tokio::test]
async fn a_panicking_adapter_is_reported_under_its_plugin() {
    let config = Config { comm: "panics".into(), framing: "length".into(), ..FakeGds::config() };
    let gds = FakeGds::start_with(&[&TestPlugin::new(false)], empty_dictionary(), config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let faults = gds.records(&[KIND_PLUGIN_FAULT]).await;
    assert_eq!(faults[0].body, json!({"plugin": "test", "message": "adapter bug"}));
    assert_eq!(gds.core.status.snapshot()["plugins"]["test"]["state"], "faulted");
}

#[tokio::test]
async fn stop_waits_for_handlers_then_closes_storage() {
    let gds = gds().await;
    gds.receive(&[0, 1]).await;
    let started = Instant::now();
    gds.core.stop().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1), "nothing was stuck");
    // What a handler published on its way out was written before storage closed.
    let query = Query { kinds: vec!["t.packet".into()], limit: 100, ..Default::default() };
    let storage = &gds.core.registry.storages["memory"].1;
    let stored = storage.query(&query).await.unwrap().records;
    assert_eq!(stored.last().unwrap().body, json!({"last": true}));
}

#[tokio::test]
async fn stop_does_not_wait_for_ever_for_a_stuck_handler() {
    let gds = FakeGds::start(&[&TestPlugin::new(true)], empty_dictionary()).await.unwrap();
    let started = Instant::now();
    gds.core.stop().await.unwrap();
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(2) && took < Duration::from_secs(5), "{took:?}");
}

async fn gds_with(config: Config) -> FakeGds {
    let config = Config { comm: "scripted".into(), ..config };
    FakeGds::start_with(&[&TestPlugin::new(false)], empty_dictionary(), config).await.unwrap()
}

async fn lengths(gds: &FakeGds, count: usize) -> Vec<serde_json::Value> {
    let mut stream = gds.core.records.subscribe(Filter::kinds(&["t.packet"]), Some(0));
    let mut out = Vec::new();
    while out.len() < count {
        let next = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
        out.push(next.expect("a record in time").unwrap().body["len"].clone());
    }
    out
}

#[tokio::test]
async fn without_a_framing_layer_each_read_is_a_packet_and_each_packet_a_write() {
    let gds = gds_with(Config { framing: "none".into(), ..FakeGds::config() }).await;
    gds.wait_for_link("connected").await;
    gds.link.push(&[1, 2, 3]);
    assert_eq!(lengths(&gds, 1).await, [json!(3)]);
    gds.core.uplink.send_packet(&[9, 8], &PacketMeta::default(), "tester", false).await.unwrap();
    assert_eq!(gds.link.sent(), [&[9u8, 8][..]]);
}

#[tokio::test]
async fn two_framing_layers_are_applied_outermost_first_in_both_directions() {
    let layers = vec!["length".to_string(), "length".to_string()];
    let gds = gds_with(Config { framing: "length".into(), framing_layers: layers, ..FakeGds::config() }).await;
    gds.wait_for_link("connected").await;
    // One outer frame holding two inner frames, delivered in two reads.
    let inner = [LengthFraming::frame(&[1, 2, 3]), LengthFraming::frame(&[4])].concat();
    let outer = LengthFraming::frame(&inner);
    gds.link.push(&outer[..4]);
    gds.link.push(&outer[4..]);
    assert_eq!(lengths(&gds, 2).await, [json!(3), json!(1)]);
    gds.core.uplink.send_packet(&[7], &PacketMeta::default(), "tester", false).await.unwrap();
    assert_eq!(gds.link.sent(), [LengthFraming::frame(&LengthFraming::frame(&[7]))]);
}

#[tokio::test]
async fn records_received_before_the_link_closes_are_kept_across_the_reconnection() {
    let gds = gds().await;
    gds.receive(&[0, 1]).await;
    gds.link.close();
    gds.wait_for_link("disconnected").await;
    gds.wait_for_link("connected").await;
    gds.receive(&[0, 1, 2]).await;
    assert_eq!(lengths(&gds, 2).await, [json!(2), json!(3)]);
    let states: Vec<_> = status_records(&gds).await.iter().map(|s| s["link"].clone()).collect();
    assert_eq!(states, ["connected", "disconnected", "connected"]);
}
