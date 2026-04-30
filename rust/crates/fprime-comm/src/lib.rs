//! TCP transport for the F´ ground system.
//!
//! Provides two modes:
//!
//! * **Server** (default) — Listens for an FSW connection.  This mirrors the
//!   common Python `IpAdapter(server=True)` deployment shape.
//! * **Client** — Connects to a remote FSW.  Useful when running against a
//!   simulator that listens.
//!
//! The transport surfaces:
//!
//! * a stream of *deframed* F´ packets (downlink), and
//! * a sink that takes payloads, frames them, and writes them to the wire
//!   (uplink).
//!
//! Reconnection is handled internally: if the FSW disconnects we go back to
//! listening (server mode) or retrying (client mode) and emit a `Status::Down`
//! event so the UI layer can show it.

#![deny(rust_2018_idioms)]

use std::{net::SocketAddr, time::Duration};

use anyhow::Context;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tracing::{debug, info, warn};

use fprime_frame::{deframe, frame};

const READ_BUF: usize = 4096;
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Transport mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Listen for FSW.
    Server,
    /// Connect to FSW.
    Client,
}

/// Connection-state events the transport reports out of band.
#[derive(Debug, Clone)]
pub enum Status {
    Listening(SocketAddr),
    Connected(SocketAddr),
    Down(String),
}

/// A handle returned from [`spawn`].
pub struct Comm {
    /// Receiver of deframed downlink packets.
    pub downlink: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Sender of uplink payloads (will be framed before being written).
    pub uplink: mpsc::UnboundedSender<Vec<u8>>,
    /// Receiver of connection-state changes.
    pub status: mpsc::UnboundedReceiver<Status>,
}

/// Spawn the comm task.  Returns a handle that owns three channels.
pub fn spawn(addr: SocketAddr, mode: Mode) -> Comm {
    let (dl_tx, dl_rx) = mpsc::unbounded_channel();
    let (ul_tx, ul_rx) = mpsc::unbounded_channel();
    let (st_tx, st_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        if let Err(e) = run(addr, mode, dl_tx, ul_rx, st_tx).await {
            warn!("comm task ended with error: {e:#}");
        }
    });

    Comm {
        downlink: dl_rx,
        uplink: ul_tx,
        status: st_rx,
    }
}

async fn run(
    addr: SocketAddr,
    mode: Mode,
    dl_tx: mpsc::UnboundedSender<Vec<u8>>,
    mut ul_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    st_tx: mpsc::UnboundedSender<Status>,
) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = match mode {
            Mode::Server => match accept_one(addr, &st_tx).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("listen/accept failed: {e:#}");
                    let _ = st_tx.send(Status::Down(format!("listen: {e}")));
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            },
            Mode::Client => match TcpStream::connect(addr).await {
                Ok(s) => {
                    let peer = s.peer_addr().unwrap_or(addr);
                    (s, peer)
                }
                Err(e) => {
                    warn!("connect failed: {e:#}");
                    let _ = st_tx.send(Status::Down(format!("connect: {e}")));
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            },
        };
        let _ = stream.set_nodelay(true);
        info!("FSW connected on {peer}");
        let _ = st_tx.send(Status::Connected(peer));

        let dl_tx2 = dl_tx.clone();
        let st_tx2 = st_tx.clone();
        let session_result = run_session(stream, dl_tx2, &mut ul_rx).await;
        match session_result {
            Ok(()) => {
                let _ = st_tx2.send(Status::Down("peer closed".into()));
                info!("FSW disconnected cleanly");
            }
            Err(e) => {
                let _ = st_tx2.send(Status::Down(format!("session: {e:#}")));
                warn!("session ended: {e:#}");
            }
        }
    }
}

async fn accept_one(
    addr: SocketAddr,
    st_tx: &mpsc::UnboundedSender<Status>,
) -> anyhow::Result<(TcpStream, SocketAddr)> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let local = listener.local_addr().unwrap_or(addr);
    let _ = st_tx.send(Status::Listening(local));
    info!("listening for FSW on {local}");
    let (stream, peer) = listener.accept().await?;
    Ok((stream, peer))
}

async fn run_session(
    stream: TcpStream,
    dl_tx: mpsc::UnboundedSender<Vec<u8>>,
    ul_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = stream.into_split();

    // Uplink writer: reads from ul_rx, frames and writes to socket.
    let writer = async {
        loop {
            let payload = match ul_rx.recv().await {
                Some(p) => p,
                None => return Ok::<(), anyhow::Error>(()),
            };
            let framed = match frame(&payload) {
                Ok(f) => f,
                Err(e) => {
                    warn!("uplink: refusing to frame {} bytes: {e}", payload.len());
                    continue;
                }
            };
            wr.write_all(&framed).await?;
            wr.flush().await?;
        }
    };

    // Downlink reader: pulls bytes, runs deframer, forwards packets.
    let reader = async {
        let mut buf = [0u8; READ_BUF];
        let mut pool: Vec<u8> = Vec::with_capacity(READ_BUF * 4);
        loop {
            let n = rd.read(&mut buf).await?;
            if n == 0 {
                return Ok::<(), anyhow::Error>(());
            }
            pool.extend_from_slice(&buf[..n]);
            // Drain as many frames as available.
            loop {
                let result = deframe(&pool);
                if !result.discarded.is_empty() {
                    debug!("discarded {} bytes resyncing", result.discarded.len());
                }
                match result.frame {
                    Some(payload) => {
                        pool.drain(..result.consumed);
                        if dl_tx.send(payload).is_err() {
                            return Ok(());
                        }
                    }
                    None => {
                        if result.consumed > 0 {
                            pool.drain(..result.consumed);
                        }
                        break;
                    }
                }
            }
        }
    };

    tokio::select! {
        r = writer => r?,
        r = reader => r?,
    }
    Ok(())
}
