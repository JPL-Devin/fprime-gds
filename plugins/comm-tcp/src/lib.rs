//! Link adapters `tcp-client` (the GDS connects to the flight software) and `tcp-server`
//! (the GDS listens and the flight software connects; one connection at a time).

use async_trait::async_trait;
use bytes::BytesMut;
use fgds_core::{
    Adapter, Error, LinkReader, LinkWriter, Manifest, OptionSpec, PeerInfo, Plugin, PluginConfig,
    Registry, Result,
};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

pub struct TcpPlugin;

impl Plugin for TcpPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "comm-tcp",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![OptionSpec {
                name: "tcp-address",
                help: "Address to listen on (tcp-server) or to connect to (tcp-client)",
                default: Some("0.0.0.0:50000"),
                secret: false,
            }],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_adapter("tcp-client", Arc::new(|cfg| Ok(Box::new(Client { address: address(cfg) }))))?;
        reg.add_adapter("tcp-server", Arc::new(|cfg| Ok(Box::new(Server { address: address(cfg), listener: None }))))
    }
}

fn address(cfg: &PluginConfig) -> String {
    cfg.get("tcp-address").unwrap_or_else(|| "0.0.0.0:50000".into())
}

type Halves = (Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo);

fn halves(stream: TcpStream, address: String) -> Halves {
    let (reader, writer) = stream.into_split();
    let peer = PeerInfo { address, message_boundaries: false };
    (Box::new(Reader(reader)), Box::new(Writer(writer)), peer)
}

struct Client {
    address: String,
}

#[async_trait]
impl Adapter for Client {
    async fn connect(&mut self) -> Result<Halves> {
        let stream = TcpStream::connect(&self.address).await.map_err(|e| io(&self.address, e))?;
        Ok(halves(stream, self.address.clone()))
    }
}

struct Server {
    address: String,
    /// Kept between connections so the port stays bound.
    listener: Option<TcpListener>,
}

#[async_trait]
impl Adapter for Server {
    async fn connect(&mut self) -> Result<Halves> {
        let listener = match self.listener.take() {
            Some(listener) => listener,
            None => TcpListener::bind(&self.address).await.map_err(|e| io(&self.address, e))?,
        };
        let accepted = listener.accept().await;
        self.listener = Some(listener);
        let (stream, peer) = accepted.map_err(|e| io(&self.address, e))?;
        Ok(halves(stream, peer.to_string()))
    }
}

fn io(address: &str, e: std::io::Error) -> Error {
    Error::other(format!("tcp {address}: {e}"))
}

struct Reader(OwnedReadHalf);

#[async_trait]
impl LinkReader for Reader {
    async fn read(&mut self, buf: &mut BytesMut) -> Result<usize> {
        self.0.read_buf(buf).await.map_err(Error::Io)
    }
}

struct Writer(OwnedWriteHalf);

#[async_trait]
impl LinkWriter for Writer {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.0.write_all(data).await.map_err(Error::Io)
    }
}
