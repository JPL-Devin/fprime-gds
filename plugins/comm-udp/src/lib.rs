//! Link adapter `udp`: datagrams both ways, as `fprime-gds --communication-selection udp`. Frames go to the
//! flight software at `address`; its datagrams are received on `recv-port`, bound on every interface.

use async_trait::async_trait;
use bytes::BytesMut;
use fgds_core::{
    Adapter, Error, LinkReader, LinkWriter, Manifest, OptionSpec, PeerInfo, Plugin, PluginConfig, Registry, Result,
};
use std::sync::Arc;
use tokio::net::UdpSocket;

pub struct UdpPlugin;

impl Plugin for UdpPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "comm-udp",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![
                OptionSpec {
                    name: "address",
                    help: "Address and port of the flight software, to which the udp adapter sends",
                    default: Some("127.0.0.1:50000"),
                    secret: false,
                },
                OptionSpec {
                    name: "recv-port",
                    help: "Local port on which the udp adapter receives from the flight software",
                    default: Some("50001"),
                    secret: false,
                },
            ],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_option_check(Arc::new(|cfg, _| {
            let address = cfg.get("address").unwrap_or_default();
            fgds_core::check_address("comm-udp address", &address)?;
            if address.ends_with(":0") {
                return Err(Error::other(format!("comm-udp address: {address:?} names no port to send to")));
            }
            port(cfg).map(|_| ())
        }));
        reg.add_adapter("udp", Arc::new(|cfg| Ok(Box::new(Udp::bind(cfg)?))))
    }
}

struct Udp {
    address: String,
    /// Bound when the adapter is built, so a port in use stops startup.
    receive: Arc<UdpSocket>,
    send: Arc<UdpSocket>,
}

impl Udp {
    /// Must be called inside the runtime.
    fn bind(cfg: &PluginConfig) -> Result<Udp> {
        let address = cfg.get("address").unwrap_or_default();
        let port = port(cfg)?;
        let socket = |at: (&str, u16)| -> std::io::Result<Arc<UdpSocket>> {
            let socket = std::net::UdpSocket::bind(at)?;
            socket.set_nonblocking(true)?;
            Ok(Arc::new(UdpSocket::from_std(socket)?))
        };
        let receive = socket(("0.0.0.0", port)).map_err(|e| Error::other(format!("udp port {port}: {e}")))?;
        let send = socket(("0.0.0.0", 0)).map_err(|e| Error::other(format!("udp: {e}")))?;
        Ok(Udp { address, receive, send })
    }
}

fn port(cfg: &PluginConfig) -> Result<u16> {
    let text = cfg.get("recv-port").unwrap_or_default();
    match text.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(Error::other(format!("comm-udp recv-port: {text:?} is not a port (1 to 65535)"))),
    }
}

#[async_trait]
impl Adapter for Udp {
    async fn connect(&mut self) -> Result<(Box<dyn LinkReader>, Box<dyn LinkWriter>, PeerInfo)> {
        // No connection exists: the address is resolved, so a name that does not resolve is reported.
        let to = tokio::net::lookup_host(&self.address)
            .await
            .ok()
            .and_then(|mut found| found.find(|a| a.is_ipv4()))
            .ok_or_else(|| Error::other(format!("udp {}: no such address", self.address)))?;
        // As the Python adapter, the datagrams are read as one stream of bytes.
        let peer = PeerInfo { address: format!("udp {}", self.address), message_boundaries: false };
        Ok((Box::new(Reader(self.receive.clone())), Box::new(Writer(self.send.clone(), to)), peer))
    }
}

struct Reader(Arc<UdpSocket>);

#[async_trait]
impl LinkReader for Reader {
    async fn read(&mut self, buf: &mut BytesMut) -> Result<usize> {
        // Room for the largest datagram: a shorter buffer would cut it.
        buf.reserve(65535);
        loop {
            // An empty datagram is not the end of the link.
            match self.0.recv_buf(buf).await.map_err(Error::Io)? {
                0 => continue,
                read => return Ok(read),
            }
        }
    }
}

struct Writer(Arc<UdpSocket>, std::net::SocketAddr);

#[async_trait]
impl LinkWriter for Writer {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.0.send_to(data, self.1).await.map(|_| ()).map_err(Error::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(address: &str, port: &str) -> Result<Udp> {
        let mut cfg = PluginConfig::default();
        cfg.set("address", address);
        cfg.set("recv-port", port);
        Udp::bind(&cfg)
    }

    #[tokio::test]
    async fn frames_go_to_the_address_and_datagrams_are_read_from_the_port() {
        let flight = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let free = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut udp = adapter(&flight.local_addr().unwrap().to_string(), &free.to_string()).unwrap();
        let (mut reader, mut writer, peer) = udp.connect().await.unwrap();
        assert!(!peer.message_boundaries);
        writer.write(b"up").await.unwrap();
        let mut got = [0u8; 16];
        let (n, _) = flight.recv_from(&mut got).await.unwrap();
        assert_eq!(&got[..n], b"up");
        flight.send_to(b"", ("127.0.0.1", free)).await.unwrap();
        flight.send_to(b"down", ("127.0.0.1", free)).await.unwrap();
        let mut buf = BytesMut::new();
        assert_eq!(reader.read(&mut buf).await.unwrap(), 4);
        assert_eq!(&buf[..], b"down");
        // A second adapter on the same port is refused when it is built.
        assert!(adapter("127.0.0.1:1", &free.to_string()).is_err());
    }

    #[tokio::test]
    async fn a_port_that_is_none_is_refused() {
        for port in ["0", "65536", "x", ""] {
            assert!(adapter("127.0.0.1:1", port).is_err(), "{port}");
        }
    }
}
