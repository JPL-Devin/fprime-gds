//! Link adapters `tcp-client` (the GDS connects to the flight software) and `tcp-server`
//! (the GDS listens and the flight software connects; one connection at a time).

use async_trait::async_trait;
use bytes::BytesMut;
use fgds_core::{
    Adapter, Error, LinkReader, LinkWriter, Manifest, OptionSpec, PeerInfo, Plugin, PluginConfig, Registry, Result,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

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
            }, OptionSpec {
                name: "keepalive-interval",
                help: "Seconds between the keep-alive bytes written to the flight software, as the Python GDS; 0 writes none",
                default: Some("0.5"),
                secret: false,
            }],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_option_check(Arc::new(|cfg, _| {
            fgds_core::check_address("comm-tcp address", &address(cfg))?;
            keepalive(cfg).map(|_| ())
        }));
        reg.add_adapter(
            "tcp-client",
            Arc::new(|cfg| Ok(Box::new(Client { address: address(cfg), keepalive: keepalive(cfg)? }))),
        )?;
        reg.add_adapter("tcp-server", Arc::new(|cfg| Ok(Box::new(Server::bind(address(cfg), keepalive(cfg)?)?))))
    }
}

fn address(cfg: &PluginConfig) -> String {
    cfg.get("address").unwrap_or_default()
}

/// What the IP adapter of the Python GDS writes, unframed, every interval while connected
/// (`IpAdapter.KEEPALIVE_DATA`): a write that fails tells it the peer left. The flight software
/// deframer discards the bytes.
const KEEPALIVE: &[u8] = b"sitting well";

fn keepalive(cfg: &PluginConfig) -> Result<Duration> {
    // Without the option (a registry built by hand) nothing is written.
    let text = cfg.get("keepalive-interval").unwrap_or_else(|| "0".into());
    match text.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds >= 0.0 => Ok(Duration::from_secs_f64(seconds)),
        _ => Err(Error::other(format!("comm-tcp keepalive-interval: {text:?} is not a number of seconds"))),
    }
}

type Halves = (Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo);

/// A packet leaves when it is written: without `TCP_NODELAY` the kernel holds a small write back
/// while the previous one is not acknowledged (Nagle), up to the 40 ms of a delayed acknowledgement.
fn halves(stream: TcpStream, address: String, keepalive: Duration) -> Halves {
    let _ = stream.set_nodelay(true);
    let (reader, writer) = stream.into_split();
    let peer = PeerInfo { address, message_boundaries: false };
    (Box::new(Reader(reader, None)), Box::new(Writer::new(writer, keepalive)), peer)
}

struct Client {
    address: String,
    keepalive: Duration,
}

#[async_trait]
impl Adapter for Client {
    async fn connect(&mut self) -> Result<Halves> {
        let stream = TcpStream::connect(&self.address).await.map_err(|e| io(&self.address, e))?;
        Ok(halves(stream, self.address.clone(), self.keepalive))
    }
}

struct Server {
    address: String,
    /// Bound when the adapter is built, so a port in use or a bad address stops startup.
    listener: Arc<TcpListener>,
    /// Refuses other peers while one is connected.
    refusing: Option<tokio::task::JoinHandle<()>>,
    keepalive: Duration,
}

impl Server {
    /// Must be called inside the runtime.
    fn bind(address: String, keepalive: Duration) -> Result<Server> {
        let listener = std::net::TcpListener::bind(&address).map_err(|e| io(&address, e))?;
        listener.set_nonblocking(true).map_err(|e| io(&address, e))?;
        let listener = TcpListener::from_std(listener).map_err(|e| io(&address, e))?;
        Ok(Server { address, listener: Arc::new(listener), refusing: None, keepalive })
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
        let _ = stream.set_nodelay(true);
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
        Ok((Box::new(Reader(reader, Some(connected))), Box::new(Writer::new(writer, self.keepalive)), info))
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

/// The keep-alive task shares the socket: a frame and the keep-alive bytes never interleave.
struct Writer {
    socket: Arc<Mutex<OwnedWriteHalf>>,
    keepalive: Option<tokio::task::JoinHandle<()>>,
}

impl Writer {
    fn new(socket: OwnedWriteHalf, interval: Duration) -> Writer {
        let socket = Arc::new(Mutex::new(socket));
        let keepalive = (!interval.is_zero()).then(|| {
            let socket = socket.clone();
            tokio::spawn(async move {
                while socket.lock().await.write_all(KEEPALIVE).await.is_ok() {
                    tokio::time::sleep(interval).await;
                }
            })
        });
        Writer { socket, keepalive }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(task) = &self.keepalive {
            task.abort();
        }
    }
}

#[async_trait]
impl LinkWriter for Writer {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.socket.lock().await.write_all(data).await.map_err(Error::Io)
    }
}
