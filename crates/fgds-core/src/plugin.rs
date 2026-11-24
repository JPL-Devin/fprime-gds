//! The extension points (section 4.3 of the design).

use crate::{
    Config, Context, Filter, MetaValue, NewRecord, PacketMeta, Page, PluginConfig, Query, Record, RecordStream,
    Registry, Result, RunInfo, StorageConfig,
};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use fgds_dictionary::Dictionary;
use std::process::ExitCode;
use std::sync::Arc;

/// One option of a plugin. Option `name` of plugin `p` is `--p-name` on the command line,
/// `[plugins.p] name` in the configuration file and `name` in `PluginConfig` (section 4.11).
#[derive(Debug, Clone)]
pub struct OptionSpec {
    /// Unique within the plugin.
    pub name: &'static str,
    pub help: &'static str,
    pub default: Option<&'static str>,
    /// Redacted wherever the configuration is displayed.
    pub secret: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub name: &'static str,
    pub version: &'static str,
    pub options: Vec<OptionSpec>,
    /// Kinds another plugin must register for this one to work.
    pub requires_kinds: Vec<&'static str>,
}

pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;
    /// Declare implementations; no I/O here.
    fn register(&self, reg: &mut Registry) -> Result<()>;
}

/// What a check of options is given besides the options: known once the dictionary and the framing are.
pub struct Startup<'a> {
    pub dictionary: &'a Dictionary,
    /// The largest packet the configured framing layers carry; `None` when they set no limit.
    pub max_packet: Option<usize>,
}

pub type OptionCheck = Arc<dyn Fn(&PluginConfig, &Startup) -> Result<()> + Send + Sync>;

/// Refuse a directory option that is empty, or names a file or a place under a file: it could never be
/// created. A directory that does not exist yet is fine.
pub fn check_directory(option: &str, text: &str) -> Result<()> {
    let refuse = |why: String| Err(crate::Error::Config(format!("{option}: {why}")));
    if text.is_empty() {
        return refuse("is empty".into());
    }
    match std::path::Path::new(text).ancestors().find(|path| path.exists()) {
        Some(found) if !found.is_dir() => {
            refuse(format!("{text:?} cannot be a directory: {} is a file", found.display()))
        }
        _ => Ok(()),
    }
}

/// Refuse an address that is not `host:port`.
pub fn check_address(option: &str, text: &str) -> Result<()> {
    match text.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => Ok(()),
        _ => Err(crate::Error::Config(format!("{option}: {text:?} is not an address (host:port, port 0 to 65535)"))),
    }
}

// Link

#[derive(Debug, Clone, Default)]
pub struct PeerInfo {
    pub address: String,
    /// True when each read is one whole message (UDP).
    pub message_boundaries: bool,
}

#[async_trait]
pub trait Adapter: Send {
    /// Returns two independent halves: the core reads and writes from two tasks.
    async fn connect(&mut self) -> Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo)>;
}

#[async_trait]
pub trait LinkReader: Send {
    /// Append to `buf`; 0 means the link closed.
    async fn read(&mut self, buf: &mut BytesMut) -> Result<usize>;
}

#[async_trait]
pub trait LinkWriter: Send {
    async fn write(&mut self, data: &[u8]) -> Result<()>;
}

pub trait FramingLayer: Send + Sync {
    /// Called once at startup: a layer that needs the dictionary returns the layer to
    /// use in its place.
    fn with_dictionary(&self, _dict: &Dictionary) -> Option<Arc<dyn FramingLayer>> {
        None
    }
    fn deframer(&self, cfg: &PluginConfig, peer: &PeerInfo) -> Box<dyn Deframe>;
    fn framer(&self, cfg: &PluginConfig) -> Box<dyn Frame>;
    /// A framer whose frames are never sent, used to find the largest packet the layer carries. A layer whose
    /// framers share state (a count of the frames sent) gives one here that does not share it.
    fn trial_framer(&self, cfg: &PluginConfig) -> Box<dyn Frame> {
        self.framer(cfg)
    }
}

/// The packets a layer emits are the input stream of the next layer. A layer that
/// demultiplexes (virtual channels, application identifiers) owns the reassembly: it
/// emits whole inner packets, each with the metadata of its own channel.
pub trait Deframe: Send {
    /// Consume whole frames from the front of `input`; leave an incomplete frame in place.
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()>;
}

pub trait Frame: Send {
    /// `meta` is what the sender says about the packet (an application identifier, a
    /// virtual channel); empty when it says nothing.
    fn frame(&mut self, packet: &[u8], meta: &PacketMeta, out: &mut BytesMut) -> Result<()>;
}

pub enum Deframed {
    Packet { bytes: Bytes, meta: PacketMeta },
    Discarded { bytes: Bytes, reason: String },
}

// Data

/// The F Prime packet type field (`FwPacketDescriptorType`) at the start of a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketType(pub u32);

impl PacketType {
    pub const COMMAND: PacketType = PacketType(0);
    pub const TELEMETRY: PacketType = PacketType(1);
    pub const EVENT: PacketType = PacketType(2);
    pub const FILE: PacketType = PacketType(3);
    pub const PACKETIZED_TELEMETRY: PacketType = PacketType(4);
}

/// What the router sends to a decoder: every field given must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PacketMatch {
    pub packet_type: Option<PacketType>,
    pub meta: Vec<(String, MetaValue)>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct DecodeError(pub String);

pub trait Decoder: Send + Sync {
    fn matches(&self) -> PacketMatch;
    /// `packet` starts with the packet type field. The decoder sets `raw` of each record
    /// to that item's own bytes, in the layout its kind describes. The router fills in
    /// `meta` when the decoder leaves it empty.
    fn decode(&self, packet: &[u8], meta: &PacketMeta, dict: &Dictionary) -> Result<Vec<NewRecord>, DecodeError>;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub String);

/// A validated request: the packet to send and the content of its record.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub packet: Bytes,
    pub id: Option<u32>,
    pub body: serde_json::Value,
    /// Given to the framing layers with the packet; empty when there is nothing to say.
    pub meta: PacketMeta,
}

pub trait Encoder: Send + Sync {
    /// The request kind this encoder serves, e.g. `command`.
    fn request_kind(&self) -> &'static str;
    /// The kind of the record published for every send, accepted or not.
    fn record_kind(&self) -> &'static str;
    fn encode(&self, request: &serde_json::Value, dict: &Dictionary) -> Result<Encoded, ValidationError>;
}

#[async_trait]
pub trait Handler: Send {
    fn subscription(&self) -> Filter;
    async fn run(self: Box<Self>, ctx: Context, records: RecordStream) -> Result<()>;
}

#[async_trait]
pub trait Storage: Send + Sync {
    /// Open the store and begin a new run. With `cfg.read_only`, begin none and return
    /// the last run of the store.
    async fn open(&self, cfg: &StorageConfig) -> Result<RunInfo>;
    /// Store records in `seq` order. A record whose `seq` is already stored is left as
    /// it is: the pipeline writes a batch again when it cannot tell whether it was stored.
    async fn append(&self, batch: &[Record]) -> Result<()>;
    /// Make `through` the highest sequence number that may have been handed out: the next
    /// run starts after it (STO-14). A storage that does not outlive the process ignores it.
    async fn reserve(&self, _through: u64) -> Result<()> {
        Ok(())
    }
    /// The dictionary JSON of a run (STO-4, STO-5); `None` when the run is not known.
    async fn dictionary(&self, _run: u32) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    async fn query(&self, q: &Query) -> Result<Page>;
    /// The latest matching record per entity (kinds with entities) or per identifier.
    async fn latest(&self, q: &Query) -> Result<Page>;
    async fn runs(&self) -> Result<Vec<RunInfo>>;
    async fn close(&self) -> Result<()>;
}

// Front ends

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Declared access of one route; enforced when access control is added (section 3.9).
#[derive(Debug, Clone)]
pub struct RouteAccess {
    pub method: &'static str,
    pub path: &'static str,
    pub access: Access,
}

/// Routes served under `/api/v1/<prefix>`.
pub struct Routes {
    pub prefix: &'static str,
    pub router: axum::Router<Context>,
    pub access: Vec<RouteAccess>,
}

/// Files compiled into a plugin crate.
pub trait EmbeddedFiles: Send + Sync {
    fn get(&self, path: &str) -> Option<&'static [u8]>;
}

/// Embedded files as a list of `(path, content)`, e.g. from `include_bytes!`.
pub struct StaticFiles(pub &'static [(&'static str, &'static [u8])]);

impl EmbeddedFiles for StaticFiles {
    fn get(&self, path: &str) -> Option<&'static [u8]> {
        self.0.iter().find(|(p, _)| *p == path).map(|(_, c)| *c)
    }
}

/// A browser tab: an ES module served at `/ui/plugins/<tab_id>/<module>`.
#[derive(Clone, Copy)]
pub struct UiBundle {
    pub tab_id: &'static str,
    /// `None`: no tab; the module is loaded for its `setup` only (a renderer, an input).
    pub title: Option<&'static str>,
    pub order: u32,
    pub module: &'static str,
    pub files: &'static dyn EmbeddedFiles,
}

pub trait Subcommand: Send + Sync {
    fn command(&self) -> clap::Command;
    /// For a subcommand that stands for a command of `fprime-cli`: its options as argparse has them.
    /// Its command line is then read by `argparse::Parser::canonical` before clap sees it.
    fn argparse(&self) -> Option<crate::argparse::Parser> {
        None
    }
    /// The options of the core and of the plugins (`dictionary`, `bind`, `files-chunk-size`, ...) this
    /// subcommand honours. Any other one given on its command line is refused: it configures `fgds run`,
    /// and would be accepted here without effect.
    fn honours(&self) -> &'static [&'static str] {
        &[]
    }
    fn run(&self, args: &clap::ArgMatches, env: &CliEnv) -> Result<ExitCode>;
}

/// Where a running GDS is reached by online command-line tools.
#[derive(Debug, Clone)]
pub struct ApiClient {
    pub base_url: String,
}

pub struct CliEnv<'a> {
    pub config: &'a Config,
    pub registry: &'a Registry,
    pub dictionary: &'a dyn Fn() -> Result<Arc<Dictionary>>,
    /// Open the configured database read-only, for offline tools: the storage and its last run.
    pub storage: &'a dyn Fn() -> Result<(Arc<dyn Storage>, RunInfo)>,
    pub client: &'a dyn Fn() -> Result<ApiClient>,
}

pub type AdapterFactory = Arc<dyn Fn(&PluginConfig) -> Result<Box<dyn Adapter>> + Send + Sync>;
pub type HandlerFactory = Arc<dyn Fn(&PluginConfig) -> Result<Box<dyn Handler>> + Send + Sync>;
pub type DecoderFactory = Arc<dyn Fn(&PluginConfig) -> Result<Box<dyn Decoder>> + Send + Sync>;
pub type EncoderFactory = Arc<dyn Fn(&PluginConfig) -> Result<Box<dyn Encoder>> + Send + Sync>;
