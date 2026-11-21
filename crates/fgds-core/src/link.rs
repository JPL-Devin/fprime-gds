use crate::supervisor::panic_message;
use crate::{
    Adapter, Deframe, Deframed, Frame, FramingLayer, NewRecord, PacketMeta, PeerInfo,
    PluginConfig, Records, Result, Router, ShutdownToken, Status, KIND_DISCARDED,
};
use bytes::{Bytes, BytesMut};
use fgds_dictionary::Dictionary;
use serde_json::json;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};

/// Wait between connection attempts (LNK-6).
const RETRY: Duration = Duration::from_secs(1);
/// Longest a write may take before the send fails (section 4.5).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum LinkError {
    #[error("link down")]
    Down,
    #[error("{0}")]
    Failed(String),
}

/// One framing layer of the active profile and its options.
pub type Layer = (Arc<dyn FramingLayer>, PluginConfig);

struct Shared {
    layers: Vec<Layer>,
    /// Send-direction state, innermost layer first.
    framers: std::sync::Mutex<Vec<Box<dyn Frame>>>,
    writer: Mutex<Option<Box<dyn crate::LinkWriter>>>,
    enabled: watch::Sender<bool>,
    status: Status,
}

/// The flight link: one adapter and the framing profile. Cheap to clone.
#[derive(Clone)]
pub struct Link(Arc<Shared>);

impl Link {
    /// A link that is down until `start`. `layers` are outermost (nearest the link) first.
    pub fn new(layers: Vec<Layer>, status: Status) -> Link {
        let framers = layers.iter().rev().map(|(l, cfg)| l.framer(cfg)).collect();
        status.set_link("disconnected", None);
        Link(Arc::new(Shared {
            layers,
            framers: std::sync::Mutex::new(framers),
            writer: Mutex::new(None),
            enabled: watch::channel(true).0,
            status,
        }))
    }

    /// Disable or enable the link (LNK-22). Disabled: the adapter is closed and not retried.
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.send_replace(enabled);
    }

    /// Frame a packet and write it. Returns the bytes written to the link.
    pub async fn write_packet(&self, packet: &[u8]) -> Result<Bytes, LinkError> {
        let mut writer = self.0.writer.lock().await;
        let Some(writer) = writer.as_mut() else {
            return Err(LinkError::Down);
        };
        let mut data = BytesMut::from(packet);
        {
            let mut framers = self.0.framers.lock().unwrap_or_else(|e| e.into_inner());
            for framer in framers.iter_mut() {
                let mut out = BytesMut::new();
                framer.frame(&data, &mut out).map_err(|e| LinkError::Failed(e.to_string()))?;
                data = out;
            }
        }
        match tokio::time::timeout(WRITE_TIMEOUT, writer.write(&data)).await {
            Ok(Ok(())) => Ok(data.freeze()),
            Ok(Err(e)) => Err(LinkError::Failed(e.to_string())),
            Err(_) => Err(LinkError::Failed("write timed out".into())),
        }
    }

    /// Start the link task. The adapter is rebuilt and restarted if its task panics (MNT-7).
    pub fn start(
        &self,
        make_adapter: impl Fn() -> Result<Box<dyn Adapter>> + Send + 'static,
        router: Arc<Router>,
        records: Records,
        dict: Arc<Dictionary>,
        mut shutdown: ShutdownToken,
    ) {
        let link = self.clone();
        tokio::spawn(async move {
            while !shutdown.is_cancelled() {
                let run = make_adapter().map(|adapter| {
                    let downlink = Downlink {
                        link: link.clone(),
                        router: router.clone(),
                        records: records.clone(),
                        dict: dict.clone(),
                    };
                    tokio::spawn(downlink.run(adapter, shutdown.clone()))
                });
                let fault = match run {
                    Ok(task) => match task.await {
                        Ok(()) => return,
                        Err(e) if e.is_panic() => panic_message(&e.into_panic()),
                        Err(_) => return,
                    },
                    Err(e) => e.to_string(),
                };
                *link.0.writer.lock().await = None;
                link.0.status.set_link("disconnected", None);
                crate::report_fault(&records, &link.0.status, "adapter", &fault).await;
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    _ = shutdown.cancelled() => {}
                }
            }
        });
    }
}

/// The receive direction: read, deframe, route, publish.
struct Downlink {
    link: Link,
    router: Arc<Router>,
    records: Records,
    dict: Arc<Dictionary>,
}

impl Downlink {
    async fn run(self, mut adapter: Box<dyn Adapter>, mut shutdown: ShutdownToken) {
        let shared = &self.link.0;
        let mut enabled = shared.enabled.subscribe();
        loop {
            if !*enabled.borrow_and_update() {
                shared.status.set_link("disabled", None);
                tokio::select! {
                    _ = enabled.changed() => continue,
                    _ = shutdown.cancelled() => return,
                }
            }
            let connected = tokio::select! {
                c = adapter.connect() => c,
                _ = enabled.changed() => continue,
                _ = shutdown.cancelled() => return,
            };
            match connected {
                Ok((mut reader, writer, peer)) => {
                    *shared.writer.lock().await = Some(writer);
                    shared.status.set_link("connected", Some(peer.address.clone()));
                    let mut chain = Chain::new(&shared.layers, &peer);
                    let mut buf = BytesMut::new();
                    loop {
                        let read = tokio::select! {
                            r = reader.read(&mut buf) => r,
                            _ = enabled.changed() => break,
                            _ = shutdown.cancelled() => return,
                        };
                        match read {
                            Ok(n) if n > 0 => {
                                shared.status.count("bytes_received", n as u64);
                                chain.input(0).extend_from_slice(&buf.split());
                                self.publish(chain.run(peer.message_boundaries)).await;
                            }
                            Ok(_) => break,
                            Err(e) => {
                                tracing::warn!(error = %e, "link read failed");
                                break;
                            }
                        }
                    }
                    *shared.writer.lock().await = None;
                    shared.status.set_link("disconnected", None);
                }
                Err(e) => tracing::debug!(error = %e, "link connect failed"),
            }
            if *enabled.borrow() {
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    _ = enabled.changed() => {}
                    _ = shutdown.cancelled() => return,
                }
            }
        }
    }

    async fn publish(&self, items: Vec<Deframed>) {
        let status = &self.link.0.status;
        for item in items {
            let records = match item {
                Deframed::Packet { bytes, meta } => {
                    status.count("packets_received", 1);
                    self.router.route(&bytes, &meta, &self.dict)
                }
                Deframed::Discarded { bytes, reason } => {
                    status.count("bytes_discarded", bytes.len() as u64);
                    vec![NewRecord {
                        raw: bytes,
                        ..NewRecord::new(KIND_DISCARDED, json!({"reason": reason}))
                    }]
                }
            };
            for record in records {
                status.count(&format!("kind.{}", record.kind), 1);
                if let Err(e) = self.records.publish(record).await {
                    tracing::error!(error = %e, "record not published");
                }
            }
        }
    }
}

/// The deframers of the profile, each with its own input buffer. Packets of one layer
/// are the input of the next; the last layer's packets go to the router.
struct Chain<'a> {
    layers: &'a [Layer],
    peer: &'a PeerInfo,
    deframers: Vec<Box<dyn Deframe>>,
    inputs: Vec<BytesMut>,
}

impl<'a> Chain<'a> {
    fn new(layers: &'a [Layer], peer: &'a PeerInfo) -> Self {
        Chain {
            layers,
            peer,
            deframers: layers.iter().map(|(l, cfg)| l.deframer(cfg, peer)).collect(),
            inputs: vec![BytesMut::new(); layers.len()],
        }
    }

    fn input(&mut self, level: usize) -> &mut BytesMut {
        &mut self.inputs[level]
    }

    /// Deframe what the link delivered. With message boundaries, whatever the first
    /// layer leaves of the message is discarded so framing recovers at the next one.
    fn run(&mut self, message_boundaries: bool) -> Vec<Deframed> {
        let mut out = Vec::new();
        if self.layers.is_empty() {
            let bytes = self.inputs.first_mut().map(|b| b.split().freeze()).unwrap_or_default();
            out.push(Deframed::Packet { bytes, meta: PacketMeta::default() });
            return out;
        }
        self.level(0, &PacketMeta::default(), &mut out);
        if message_boundaries && !self.inputs[0].is_empty() {
            let bytes = self.inputs[0].split().freeze();
            out.push(Deframed::Discarded { bytes, reason: "incomplete frame in message" });
        }
        out
    }

    fn level(&mut self, level: usize, outer: &PacketMeta, out: &mut Vec<Deframed>) {
        let mut produced = Vec::new();
        let deframer = &mut self.deframers[level];
        let input = &mut self.inputs[level];
        let result = catch_unwind(AssertUnwindSafe(|| deframer.deframe(input, &mut produced)));
        if !matches!(result, Ok(Ok(()))) {
            // An error or a panic: drop the input and rebuild the layer (LNK-8).
            let bytes = input.split().freeze();
            out.push(Deframed::Discarded { bytes, reason: "framing layer failed" });
            let (layer, cfg) = &self.layers[level];
            self.deframers[level] = layer.deframer(cfg, self.peer);
        }
        for item in produced {
            match item {
                Deframed::Packet { bytes, meta } => {
                    let mut merged = outer.clone();
                    merged.extend(meta);
                    if level + 1 == self.layers.len() {
                        out.push(Deframed::Packet { bytes, meta: merged });
                    } else {
                        self.inputs[level + 1].extend_from_slice(&bytes);
                        self.level(level + 1, &merged, out);
                    }
                }
                discarded => out.push(discarded),
            }
        }
    }
}
