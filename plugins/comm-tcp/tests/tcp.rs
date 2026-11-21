use bytes::BytesMut;
use fgds_core::{Adapter, PluginConfig, Registry};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn adapter(name: &str, address: &str) -> Box<dyn Adapter> {
    let mut registry = Registry::new();
    registry.register(&comm_tcp::TcpPlugin).unwrap();
    let mut cfg = PluginConfig::default();
    cfg.set("address", address);
    (registry.adapters[name].1)(&cfg).unwrap()
}

#[tokio::test]
async fn client_connects_and_reports_failure_when_nobody_listens() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut client = adapter("tcp-client", &address);
    // The connection waits in the listener's queue until accepted, so one task is enough.
    let connecting = TcpStream::connect(&address).await.unwrap();
    drop(connecting);
    let accept = tokio::spawn(async move {
        listener.accept().await.unwrap(); // the probe above
        listener.accept().await.unwrap().0
    });
    let (mut reader, mut writer, _) = client.connect().await.unwrap();
    let mut peer = accept.await.unwrap();
    peer.write_all(b"down").await.unwrap();
    let mut buf = BytesMut::new();
    while buf.len() < 4 {
        assert!(reader.read(&mut buf).await.unwrap() > 0);
    }
    writer.write(b"up").await.unwrap();
    let mut up = [0; 2];
    peer.read_exact(&mut up).await.unwrap();
    assert_eq!((&buf[..], &up), (&b"down"[..], b"up"));
    // The peer closes: the read reports it, and nobody listens any more.
    drop(peer);
    assert_eq!(reader.read(&mut buf).await.unwrap(), 0);
    assert!(client.connect().await.is_err());
}

#[tokio::test]
async fn server_accepts_one_connection_after_another() {
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = probe.local_addr().unwrap().to_string();
    drop(probe);
    let mut server = adapter("tcp-server", &address);
    for _ in 0..2 {
        let address = address.clone();
        let peer = tokio::spawn(async move {
            loop {
                if let Ok(stream) = TcpStream::connect(&address).await {
                    return stream;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        });
        let (mut reader, _writer, _) = server.connect().await.unwrap();
        let mut stream = peer.await.unwrap();
        stream.write_all(b"x").await.unwrap();
        let mut buf = BytesMut::new();
        assert_eq!(reader.read(&mut buf).await.unwrap(), 1);
    }
}

#[tokio::test]
async fn a_server_address_that_cannot_be_bound_fails_when_the_adapter_is_built() {
    let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = taken.local_addr().unwrap().to_string();
    let mut registry = Registry::new();
    registry.register(&comm_tcp::TcpPlugin).unwrap();
    let build = |address: &str| {
        let mut cfg = PluginConfig::default();
        cfg.set("address", address);
        (registry.adapters["tcp-server"].1)(&cfg).map(|_| ()).map_err(|e| e.to_string())
    };
    let in_use = build(&address).unwrap_err();
    assert!(in_use.starts_with(&format!("tcp {address}: ")), "{in_use}");
    assert!(build("not an address").is_err());
}

#[tokio::test]
async fn a_second_peer_is_closed_while_the_first_is_connected() {
    let mut server = adapter("tcp-server", "127.0.0.1:0");
    // The adapter does not tell its port: bind a known one instead.
    drop(server);
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = probe.local_addr().unwrap().to_string();
    drop(probe);
    server = adapter("tcp-server", &address);

    let mut first = TcpStream::connect(&address).await.unwrap();
    let (mut reader, _writer, _) = server.connect().await.unwrap();
    // Not left waiting in the queue: closed at once, and the first is untouched.
    let mut second = TcpStream::connect(&address).await.unwrap();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(2), second.read(&mut [0; 1])).await;
    assert_eq!(closed.expect("the second peer is closed").unwrap_or(0), 0);
    first.write_all(b"x").await.unwrap();
    let mut buf = BytesMut::new();
    assert_eq!(reader.read(&mut buf).await.unwrap(), 1);

    // After the first leaves, the next peer is accepted.
    drop((reader, _writer, first));
    let mut third = TcpStream::connect(&address).await.unwrap();
    let (mut reader, _writer, _) = server.connect().await.unwrap();
    third.write_all(b"y").await.unwrap();
    assert_eq!(reader.read(&mut buf).await.unwrap(), 1);
}
