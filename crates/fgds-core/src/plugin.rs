//! The extension points (section 4.3 of the design).

use crate::{
    Config, Context, Filter, NewRecord, PacketMeta, MetaValue, Page, PluginConfig, Query, Record,
    RecordStream, Registry, Result, RunInfo, StorageConfig,
};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use fgds_dictionary::Dictionary;
use std::process::ExitCode;
use std::sync::Arc;

/// One option of a plugin, settable in `[plugins.<name>]` and as `--<name>` on the command line.
#[derive(Debug, Clone)]
pub struct OptionSpec {
    /// Unique across all plugins.
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
    fn deframer(&self, cfg: &PluginConfig, peer: &PeerInfo) -> Box<dyn Deframe>;
    fn framer(&self, cfg: &PluginConfig) -> Box<dyn Frame>;
}

pub trait Deframe: Send {
    /// Consume whole frames from the front of `input`; leave an incomplete frame in place.
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()>;
}

pub trait Frame: Send {
    fn frame(&mut self, packet: &[u8], out: &mut BytesMut) -> Result<()>;
}

pub enum Deframed {
    Packet { bytes: Bytes, meta: PacketMeta },
    Discarded { bytes: Bytes, reason: &'static str },
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
    pub meta: Vec<(&'static str, MetaValue)>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct DecodeError(pub String);

pub trait Decoder: Send + Sync {
    fn matches(&self) -> PacketMatch;
    /// `packet` starts with the packet type field. The router fills in `raw` and `meta`
    /// of the returned records when the decoder leaves them empty.
    fn decode(
        &self,
        packet: &[u8],
        meta: &PacketMeta,
        dict: &Dictionary,
    ) -> Result<Vec<NewRecord>, DecodeError>;
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
}

pub trait Encoder: Send + Sync {
    /// The request kind this encoder serves, e.g. `command`.
    fn request_kind(&self) -> &'static str;
    /// The kind of the record published for every send, accepted or not.
    fn record_kind(&self) -> &'static str;
    fn encode(
        &self,
        request: &serde_json::Value,
        dict: &Dictionary,
    ) -> Result<Encoded, ValidationError>;
}

#[async_trait]
pub trait Handler: Send {
    fn subscription(&self) -> Filter;
    async fn run(self: Box<Self>, ctx: Context, records: RecordStream) -> Result<()>;
}

#[async_trait]
pub trait Storage: Send + Sync {
    /// Open the store and begin a new run.
    async fn open(&self, cfg: &StorageConfig) -> Result<RunInfo>;
    async fn append(&self, batch: &[Record]) -> Result<()>;
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
    pub title: &'static str,
    pub order: u32,
    pub module: &'static str,
    pub files: &'static dyn EmbeddedFiles,
}

pub trait Subcommand: Send + Sync {
    fn command(&self) -> clap::Command;
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
    pub client: &'a dyn Fn() -> Result<ApiClient>,
}

pub type AdapterFactory = Arc<dyn Fn(&PluginConfig) -> Result<Box<dyn Adapter>> + Send + Sync>;
pub type HandlerFactory = Arc<dyn Fn(&PluginConfig) -> Box<dyn Handler> + Send + Sync>;
