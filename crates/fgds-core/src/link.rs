use crate::supervisor::panic_message;
use crate::{
    Adapter, Deframe, Deframed, Frame, FramingLayer, NewRecord, PacketMeta, PeerInfo, PluginConfig, Records, Result,
    Router, ShutdownToken, Status, KIND_DISCARDED, KIND_STATUS,
};
use bytes::{Bytes, BytesMut};
use fgds_dictionary::Dictionary;
use serde_json::json;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{watch, Mutex};

/// Wait between connection attempts (LNK-6).
const RETRY: Duration = Duration::from_secs(1);
/// Longest wait before an adapter that failed to start is built again.
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Room asked for before each read, so one read takes what the link has.
const READ_BUFFER: usize = 64 * 1024;
/// Longest a write may take before the send fails (section 4.5).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum LinkError {
    #[error("link down")]
    Down,
    /// A framing layer refused the packet (too large, for example): nothing was written.
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
}

/// One framing layer of the active profile and its options.
pub type Layer = (Arc<dyn FramingLayer>, PluginConfig);

struct Shared {
    layers: Vec<Layer>,
    /// Send-direction state, innermost layer first.
    framers: std::sync::Mutex<Vec<Box<dyn Frame>>>,
    max_packet: Option<usize>,
    writer: Mutex<Option<Box<dyn crate::LinkWriter>>>,
    enabled: watch::Sender<bool>,
    status: Status,
    raw_log: Option<RawLog>,
    /// Changed when a write fails: the read loop drops the connection and reconnects.
    reset: watch::Sender<u64>,
    /// Set by `start`; link state changes are published as `gds.status` records.
    records: OnceLock<Records>,
    write_timeout_ms: AtomicU64,
}

/// The raw log of the run (LNK-7): one line per chunk of bytes received or sent,
/// `<ground time> <recv|sent> <bytes in hexadecimal>`.
struct RawLog(std::sync::Mutex<std::fs::File>);

impl RawLog {
    fn write(&self, direction: &str, data: &[u8]) {
        use std::io::Write;
        let mut line = format!("{} {direction} ", crate::Timestamp::now().to_iso());
        for byte in data {
            line.push_str(&format!("{byte:02x}"));
        }
        line.push('\n');
        let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = file.write_all(line.as_bytes()) {
            tracing::warn!(error = %e, "raw log not written");
        }
    }
}

/// The largest packet `layers` (outermost first) frame, found by framing packets of zeros with framers of
/// their own, so the limit is the one the framers apply. `None`: no limit below 1 MiB, or none found.
pub fn max_packet(layers: &[Layer]) -> Option<usize> {
    let framed = |length: usize| {
        let mut data = BytesMut::zeroed(length);
        layers.iter().rev().all(|(layer, cfg)| {
            let mut out = BytesMut::new();
            let framed = layer.framer(cfg).frame(&data, &PacketMeta::default(), &mut out).is_ok();
            data = out;
            framed
        })
    };
    let (mut fits, mut over) = (16, 1 << 20);
    if !framed(fits) || framed(over) {
        return None;
    }
    while over - fits > 1 {
        let middle = (fits + over) / 2;
        match framed(middle) {
            true => fits = middle,
            false => over = middle,
        }
    }
    Some(fits)
}

/// The flight link: one adapter and the framing profile. Cheap to clone.
#[derive(Clone)]
pub struct Link(Arc<Shared>);

impl Link {
    /// A link that is down until `start`. `layers` are outermost (nearest the link) first.
    pub fn new(layers: Vec<Layer>, status: Status) -> Link {
        Self::with_raw_log(layers, status, None)
    }

    /// A link that also writes every chunk received and sent to `raw_log` (LNK-7).
    pub fn with_raw_log(layers: Vec<Layer>, status: Status, raw_log: Option<std::fs::File>) -> Link {
        let framers = layers.iter().rev().map(|(l, cfg)| l.framer(cfg)).collect();
        let max_packet = max_packet(&layers);
        status.set_link("disconnected", None);
        Link(Arc::new(Shared {
            layers,
            max_packet,
            framers: std::sync::Mutex::new(framers),
            writer: Mutex::new(None),
            enabled: watch::channel(true).0,
            status,
            raw_log: raw_log.map(|file| RawLog(std::sync::Mutex::new(file))),
            reset: watch::channel(0).0,
            records: OnceLock::new(),
            write_timeout_ms: AtomicU64::new(WRITE_TIMEOUT.as_millis() as u64),
        }))
    }

    /// The largest packet the framing layers carry (`max_packet`).
    pub fn max_packet(&self) -> Option<usize> {
        self.0.max_packet
    }

    /// Change how long a write may take (5 s unless set).
    pub fn set_write_timeout(&self, limit: Duration) {
        self.0.write_timeout_ms.store(limit.as_millis() as u64, Ordering::SeqCst);
    }

    /// Set the link state in the status and, when it changed, publish it (`gds.status`).
    fn set_state(&self, state: &str, peer: Option<String>, error: Option<String>) {
        if !self.0.status.set_link_state(state, peer.clone(), error.clone()) {
            return;
        }
        if let Some(records) = self.0.records.get() {
            let body = json!({"link": state, "peer": peer, "error": error});
            // Fails only when the pipeline is stopped.
            let _ = records.publish_uplink(NewRecord::new(KIND_STATUS, body));
        }
    }

    /// Disable or enable the link (LNK-22). Disabled: the adapter is closed and not retried.
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.send_replace(enabled);
    }

    /// Frame a packet and write it. Returns the bytes written to the link.
    pub async fn write_packet(&self, packet: &[u8], meta: &PacketMeta) -> Result<Bytes, LinkError> {
        let mut slot = self.0.writer.lock().await;
        let Some(writer) = slot.as_mut() else {
            return Err(LinkError::Down);
        };
        let mut data = BytesMut::from(packet);
        {
            let mut framers = self.0.framers.lock().unwrap_or_else(|e| e.into_inner());
            for framer in framers.iter_mut() {
                let mut out = BytesMut::new();
                framer.frame(&data, meta, &mut out).map_err(|e| LinkError::Invalid(e.to_string()))?;
                data = out;
            }
        }
        let limit = Duration::from_millis(self.0.write_timeout_ms.load(Ordering::SeqCst));
        let failure = match tokio::time::timeout(limit, writer.write(&data)).await {
            Ok(Ok(())) => {
                if let Some(log) = &self.0.raw_log {
                    log.write("sent", &data);
                }
                return Ok(data.freeze());
            }
            Ok(Err(e)) => format!("write failed: {e}"),
            Err(_) => "write timed out".to_string(),
        };
        // Part of a frame may be on the link: the connection cannot be used again.
        *slot = None;
        drop(slot);
        tracing::warn!(error = %failure, "link write failed; the connection is dropped");
        self.set_state("disconnected", None, Some(failure.clone()));
        self.0.reset.send_modify(|n| *n += 1);
        Err(LinkError::Failed(failure))
    }

    /// Start the link task for the adapter of `plugin`. The adapter is rebuilt and
    /// restarted if its task panics or its factory fails (MNT-7), after 1 s, then 2 s,
    /// up to 30 s; the fault is reported once per distinct message.
    pub fn start(
        &self,
        plugin: &str,
        mut make_adapter: impl FnMut() -> Result<Box<dyn Adapter>> + Send + 'static,
        router: Arc<Router>,
        records: Records,
        dict: Arc<Dictionary>,
        mut shutdown: ShutdownToken,
    ) -> tokio::task::JoinHandle<()> {
        let link = self.clone();
        let plugin = plugin.to_string();
        let _ = self.0.records.set(records.clone());
        tokio::spawn(async move {
            let mut wait = RETRY;
            let mut reported: Option<String> = None;
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
                link.set_state("disconnected", None, Some(fault.clone()));
                if reported.as_ref() != Some(&fault) {
                    crate::report_fault(&records, &link.0.status, &plugin, &fault).await;
                    reported = Some(fault);
                }
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = shutdown.cancelled() => {}
                }
                wait = (wait * 2).min(RETRY_MAX);
            }
        })
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
        let mut last_error: Option<String> = None;
        loop {
            if !*enabled.borrow_and_update() {
                self.link.set_state("disabled", None, None);
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
                    let mut reset = shared.reset.subscribe();
                    *shared.writer.lock().await = Some(writer);
                    last_error = None;
                    self.link.set_state("connected", Some(peer.address.clone()), None);
                    let mut chain = Chain::new(&shared.layers, &peer);
                    let mut buf = BytesMut::new();
                    loop {
                        // `split` below leaves no room: without this a read gets 64 bytes.
                        buf.reserve(READ_BUFFER);
                        let read = tokio::select! {
                            r = reader.read(&mut buf) => r,
                            _ = enabled.changed() => break,
                            // A write failed and already set the state and its reason.
                            _ = reset.changed() => break,
                            _ = shutdown.cancelled() => return,
                        };
                        match read {
                            Ok(n) if n > 0 => {
                                shared.status.count("bytes_received", n as u64);
                                shared.status.count("link_reads", 1);
                                let chunk = buf.split();
                                if let Some(log) = &shared.raw_log {
                                    log.write("recv", &chunk);
                                }
                                chain.input(0).extend_from_slice(&chunk);
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
                    if shared.status.link() == "connected" {
                        self.link.set_state("disconnected", None, None);
                    }
                }
                Err(e) => {
                    // Said once per distinct error, not once per attempt.
                    let error = e.to_string();
                    if last_error.as_ref() != Some(&error) {
                        tracing::warn!(error = %error, "link not connected; retrying every second");
                    }
                    self.link.set_state("disconnected", None, Some(error.clone()));
                    last_error = Some(error);
                }
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
                    vec![NewRecord { raw: bytes, ..NewRecord::new(KIND_DISCARDED, json!({"reason": reason})) }]
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
            // Without a layer the one buffer holds what was read.
            inputs: vec![BytesMut::new(); layers.len().max(1)],
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
            out.push(Deframed::Discarded { bytes, reason: "incomplete frame in message".into() });
        }
        out
    }

    fn level(&mut self, level: usize, outer: &PacketMeta, out: &mut Vec<Deframed>) {
        let mut produced = Vec::new();
        let deframer = &mut self.deframers[level];
        let input = &mut self.inputs[level];
        let result = catch_unwind(AssertUnwindSafe(|| deframer.deframe(input, &mut produced)));
        let failure = match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("framing layer failed: {e}")),
            Err(panic) => Some(format!("framing layer panicked: {}", panic_message(&panic))),
        };
        if let Some(reason) = failure {
            // An error or a panic: drop the input and rebuild the layer (LNK-8).
            let bytes = input.split().freeze();
            out.push(Deframed::Discarded { bytes, reason });
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
