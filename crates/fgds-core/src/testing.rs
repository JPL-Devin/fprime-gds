//! Test doubles for plugin tests (MNT-2): an in-memory storage, a scripted adapter, a
//! trivial framing layer, a core started on them, and the storage conformance suite.

use crate::{
    async_trait, Adapter, Config, Core, Deframe, Deframed, Error, Frame, FramingLayer, KindSpec, LinkReader,
    LinkWriter, Manifest, PacketMeta, Page, PeerInfo, Plugin, PluginConfig, Query, Record, Registry, Result, RunInfo,
    Storage, StorageConfig, Timestamp,
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use fgds_dictionary::Dictionary;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Default)]
struct Memory {
    run: Option<RunInfo>,
    kinds: HashMap<String, KindSpec>,
    /// In `seq` order.
    records: Vec<Record>,
    dictionary: Vec<u8>,
}

/// Storage that keeps one run in memory. The `storage-memory` plugin registers it.
#[derive(Default)]
pub struct MemoryStorage(Mutex<Memory>);

impl MemoryStorage {
    /// Matching records in `seq` order, cut to the query's page size.
    fn page(&self, q: &Query, latest_only: bool) -> Result<Page> {
        let memory = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let run = memory.run.as_ref().ok_or_else(|| Error::other("storage is not open"))?.run;
        let matching = memory.records.iter().filter(|r| q.matches(r, memory.kinds.get(&r.kind), run));
        let mut records: Vec<&Record> = if latest_only {
            // Later records replace earlier ones with the same key.
            let mut latest = BTreeMap::new();
            for record in matching {
                let entities = memory.kinds.get(&record.kind).is_some_and(|k| k.has_entities);
                let key = match entities {
                    true => (&record.kind, record.entity.clone(), None),
                    false => (&record.kind, None, record.id),
                };
                latest.insert(key, record);
            }
            let mut records: Vec<_> = latest.into_values().collect();
            records.sort_by_key(|r| r.seq);
            records
        } else {
            matching.collect()
        };
        let limit = q.limit as usize;
        let more = records.len() > limit;
        records.truncate(limit);
        Ok(Page {
            next: records.last().filter(|_| more).map(|r| r.seq),
            records: records.into_iter().cloned().collect(),
        })
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn open(&self, cfg: &StorageConfig) -> Result<RunInfo> {
        let run = RunInfo {
            run: 1,
            started: Timestamp::now(),
            gds_version: cfg.gds_version.clone(),
            dictionary_version: cfg.dictionary_version.clone(),
            plugins: cfg.plugins.clone(),
            first_seq: 1,
            reserved_seq: u64::MAX,
            dictionary_hash: cfg.dictionary_hash.clone(),
        };
        let mut memory = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *memory = Memory {
            run: Some(run.clone()),
            kinds: cfg.kinds.iter().map(|k| (k.id.clone(), k.clone())).collect(),
            records: Vec::new(),
            dictionary: cfg.dictionary_json.clone(),
        };
        Ok(run)
    }

    async fn append(&self, batch: &[Record]) -> Result<()> {
        let mut memory = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for record in batch {
            // A record already stored is left as it is.
            if let Err(at) = memory.records.binary_search_by_key(&record.seq, |r| r.seq) {
                memory.records.insert(at, record.clone());
            }
        }
        Ok(())
    }

    async fn dictionary(&self, run: u32) -> Result<Option<Vec<u8>>> {
        let memory = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Ok(memory.run.as_ref().filter(|r| r.run == run).map(|_| memory.dictionary.clone()))
    }

    async fn query(&self, q: &Query) -> Result<Page> {
        self.page(q, false)
    }

    async fn latest(&self, q: &Query) -> Result<Page> {
        self.page(q, true)
    }

    async fn runs(&self) -> Result<Vec<RunInfo>> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).run.clone().into_iter().collect())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// What the test does to the scripted link.
enum Step {
    Data(Bytes),
    Close,
}

struct Script {
    incoming: tokio::sync::Mutex<mpsc::UnboundedReceiver<Step>>,
    sent: Mutex<Vec<Bytes>>,
}

/// An adapter driven by the test: it connects at once, delivers the bytes the test
/// pushes, and keeps what the GDS writes.
pub struct ScriptedAdapter(Arc<Script>);

/// The test's end of a `ScriptedAdapter`.
#[derive(Clone)]
pub struct ScriptHandle {
    steps: mpsc::UnboundedSender<Step>,
    script: Arc<Script>,
}

impl ScriptedAdapter {
    pub fn new() -> (ScriptedAdapter, ScriptHandle) {
        let (steps, incoming) = mpsc::unbounded_channel();
        let script = Arc::new(Script { incoming: tokio::sync::Mutex::new(incoming), sent: Mutex::new(Vec::new()) });
        (ScriptedAdapter(script.clone()), ScriptHandle { steps, script })
    }
}

impl ScriptHandle {
    /// Deliver bytes to the GDS as one read.
    pub fn push(&self, data: &[u8]) {
        let _ = self.steps.send(Step::Data(Bytes::copy_from_slice(data)));
    }

    /// Close the connection; the adapter reconnects on the next attempt.
    pub fn close(&self) {
        let _ = self.steps.send(Step::Close);
    }

    /// Every write of the GDS so far.
    pub fn sent(&self) -> Vec<Bytes> {
        self.script.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// An adapter factory for `Registry::add_adapter` sharing this script.
    pub fn factory(&self) -> crate::AdapterFactory {
        let script = self.script.clone();
        Arc::new(move |_| Ok(Box::new(ScriptedAdapter(script.clone())) as Box<dyn Adapter>))
    }
}

#[async_trait]
impl Adapter for ScriptedAdapter {
    async fn connect(&mut self) -> Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo)> {
        let peer = PeerInfo { address: "scripted".into(), message_boundaries: false };
        Ok((Box::new(ScriptedAdapter(self.0.clone())), Box::new(ScriptedAdapter(self.0.clone())), peer))
    }
}

#[async_trait]
impl LinkReader for ScriptedAdapter {
    async fn read(&mut self, buf: &mut BytesMut) -> Result<usize> {
        match self.0.incoming.lock().await.recv().await {
            Some(Step::Data(data)) => {
                buf.extend_from_slice(&data);
                Ok(data.len())
            }
            Some(Step::Close) | None => Ok(0),
        }
    }
}

#[async_trait]
impl LinkWriter for ScriptedAdapter {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.0.sent.lock().unwrap_or_else(|e| e.into_inner()).push(Bytes::copy_from_slice(data));
        Ok(())
    }
}

/// A framing layer for tests: each packet is preceded by its length as a `U16`.
/// A zero length is an error, to exercise the failure path.
pub struct LengthFraming;

impl LengthFraming {
    pub fn frame(packet: &[u8]) -> Vec<u8> {
        let mut out = (packet.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(packet);
        out
    }
}

impl FramingLayer for LengthFraming {
    fn deframer(&self, _: &PluginConfig, _: &PeerInfo) -> Box<dyn Deframe> {
        Box::new(LengthFraming)
    }
    fn framer(&self, _: &PluginConfig) -> Box<dyn Frame> {
        Box::new(LengthFraming)
    }
}

impl Deframe for LengthFraming {
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()> {
        while input.len() >= 2 {
            let len = u16::from_be_bytes([input[0], input[1]]) as usize;
            if len == 0 {
                return Err(Error::other("zero-length frame"));
            }
            if input.len() < 2 + len {
                break;
            }
            input.advance(2);
            let bytes = input.split_to(len).freeze();
            out.push(Deframed::Packet { bytes, meta: PacketMeta::default() });
        }
        Ok(())
    }
}

impl Frame for LengthFraming {
    fn frame(&mut self, packet: &[u8], _: &PacketMeta, out: &mut BytesMut) -> Result<()> {
        out.put_slice(&LengthFraming::frame(packet));
        Ok(())
    }
}

/// Registers the test doubles: storage `memory`, adapter `scripted`, framing `length`.
struct Doubles(ScriptHandle);

impl Plugin for Doubles {
    fn manifest(&self) -> Manifest {
        Manifest { name: "test-doubles", version: crate::VERSION, ..Default::default() }
    }
    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_storage("memory", Arc::new(MemoryStorage::default()))?;
        reg.add_adapter("scripted", self.0.factory())?;
        reg.add_framing_layer("length", Arc::new(LengthFraming))?;
        reg.add_framing_profile("length", &["length"])
    }
}

/// A core running on the test doubles: the fake context of a plugin test.
pub struct FakeGds {
    pub core: Arc<Core>,
    /// The flight side of the link. Frames are `LengthFraming::frame(packet)`.
    pub link: ScriptHandle,
}

impl FakeGds {
    /// Register `plugins` beside the test doubles and start the core with the link up.
    pub async fn start(plugins: &[&dyn Plugin], dictionary: Dictionary) -> Result<FakeGds> {
        let (_, link) = ScriptedAdapter::new();
        let mut registry = Registry::new();
        registry.register(&Doubles(link.clone()))?;
        for plugin in plugins {
            registry.register(*plugin)?;
        }
        let config = Config { comm: "scripted".into(), framing: "length".into(), ..Default::default() };
        let core = Arc::new(Core::start(registry, config, dictionary).await?);
        core.start_link();
        let gds = FakeGds { core, link };
        gds.wait_for_link("connected").await;
        Ok(gds)
    }

    /// The context plugin `name` would receive.
    pub fn context(&self, name: &str) -> crate::Context {
        self.core.context(name)
    }

    /// Deliver one packet from the flight side and wait until its records are stored.
    pub async fn receive(&self, packet: &[u8]) {
        let before = self.core.status.snapshot()["counters"]["packets_received"].as_u64().unwrap_or(0);
        self.link.push(&LengthFraming::frame(packet));
        for _ in 0..2000 {
            let now = self.core.status.snapshot()["counters"]["packets_received"].as_u64().unwrap_or(0);
            if now > before {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        self.core.records.flush().await;
    }

    pub async fn wait_for_link(&self, state: &str) {
        for _ in 0..5000 {
            if self.core.status.link() == state {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("link did not become {state}");
    }

    /// Every stored record of the given kinds (all kinds when empty).
    pub async fn records(&self, kinds: &[&str]) -> Vec<Record> {
        self.core.records.flush().await;
        let query = Query { kinds: kinds.iter().map(|k| k.to_string()).collect(), ..Default::default() };
        self.core.records.query(query).await.expect("query").records
    }
}

/// The smallest dictionary that loads, for tests that need no definitions.
pub fn empty_dictionary() -> Dictionary {
    let text = r#"{"typeDefinitions":[],"commands":[],"events":[],"telemetryChannels":[]}"#;
    Dictionary::parse("empty", text).expect("empty dictionary")
}

pub mod conformance;
