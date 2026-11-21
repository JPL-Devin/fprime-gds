//! Link adapters `tcp-client` (the GDS connects to the flight software) and `tcp-server`
//! (the GDS listens and the flight software connects; one connection at a time).

use async_trait::async_trait;
use bytes::BytesMut;
use fgds_core::{
    Adapter, Error, LinkReader, LinkWriter, Manifest, OptionSpec, PeerInfo, Plugin, PluginConfig, Registry, Result,
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
                name: "address",
                help: "Address to listen on (tcp-server) or to connect to (tcp-client)",
                default: Some("0.0.0.0:50000"),
                secret: false,
            }],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_adapter("tcp-client", Arc::new(|cfg| Ok(Box::new(Client { address: address(cfg) }))))?;
        reg.add_adapter("tcp-server", Arc::new(|cfg| Ok(Box::new(Server::bind(address(cfg))?))))
    }
}

fn address(cfg: &PluginConfig) -> String {
    cfg.get("address").unwrap_or_default()
}

type Halves = (Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo);

fn halves(stream: TcpStream, address: String) -> Halves {
    let (reader, writer) = stream.into_split();
    let peer = PeerInfo { address, message_boundaries: false };
    (Box::new(Reader(reader, None)), Box::new(Writer(writer)), peer)
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
    /// Bound when the adapter is built, so a port in use or a bad address stops startup.
    listener: Arc<TcpListener>,
    /// Refuses other peers while one is connected.
    refusing: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    /// Must be called inside the runtime.
    fn bind(address: String) -> Result<Server> {
        let listener = std::net::TcpListener::bind(&address).map_err(|e| io(&address, e))?;
        listener.set_nonblocking(true).map_err(|e| io(&address, e))?;
        let listener = TcpListener::from_std(listener).map_err(|e| io(&address, e))?;
        Ok(Server { address, listener: Arc::new(listener), refusing: None })
    }
}

#[async_trait]
impl Adapter for Server {
    async fn connect(&mut self) -> Result<Halves> {
        // The task that refused peers during the last connection ends with it.
        if let Some(refusing) = self.refusing.take() {
            let _ = refusing.await;
        }
        let (stream, peer) = self.listener.accept().await.map_err(|e| io(&self.address, e))?;
        let (reader, writer) = stream.into_split();
        let (connected, mut closed) = tokio::sync::oneshot::channel::<()>();
        let listener = self.listener.clone();
        // One connection at a time (LNK-5): another peer is closed at once, not kept waiting.
        self.refusing = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut closed => return,
                    accepted = listener.accept() => match accepted {
                        Ok((_, other)) => tracing::warn!(peer = %other, "connection refused: the link is already connected"),
                        Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
                    },
                }
            }
        }));
        let info = PeerInfo { address: peer.to_string(), message_boundaries: false };
        Ok((Box::new(Reader(reader, Some(connected))), Box::new(Writer(writer)), info))
    }
}

fn io(address: &str, e: std::io::Error) -> Error {
    Error::other(format!("tcp {address}: {e}"))
}

/// The second field is dropped with the reader: it tells the server the connection ended.
struct Reader(OwnedReadHalf, #[allow(dead_code)] Option<tokio::sync::oneshot::Sender<()>>);

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
