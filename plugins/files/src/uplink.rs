//! The uplink queue (FIL-1, FIL-4, FIL-5): one file at a time, as file packets.

use crate::packet::{self, Checksum, FilePacket};
use crate::{publish, Options, Shared, Transfer, KIND_UPLINK};
use fgds_core::{async_trait, Context, Filter, Handler, PacketMeta, RecordStream, Result, SendError};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

/// How often the link state is looked at when nothing else happens.
const POLL: Duration = Duration::from_millis(100);

pub(crate) struct UplinkHandler {
    pub shared: Arc<Shared>,
    pub options: Options,
}

/// Why a transfer stopped before its end packet.
enum Stop {
    Canceled,
    Failed(String),
    /// The link is not connected: the queue pauses as well (FIL-4).
    LinkLost,
}

#[async_trait]
impl Handler for UplinkHandler {
    fn subscription(&self) -> Filter {
        // Its own records, read and dropped: the queue is fed by the routes.
        Filter::kinds(&[KIND_UPLINK])
    }

    async fn run(self: Box<Self>, ctx: Context, mut records: RecordStream) -> Result<()> {
        let mut shutdown = ctx.shutdown.clone();
        loop {
            if ctx.status.link() != "connected" {
                self.shared.state().paused = true;
            }
            let next = {
                let mut state = self.shared.state();
                let paused = state.paused;
                state.uplinks.iter_mut().find(|t| t.state == "queued" && !paused).map(|t| {
                    t.state = "transferring";
                    t.clone()
                })
            };
            // Its own progress records are read so that the subscription never lags.
            while let Ok(Some(_)) = tokio::time::timeout(Duration::ZERO, records.next()).await {}
            let Some(transfer) = next else {
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = self.shared.wake.notified() => {}
                    record = records.next() => if record.is_none() { return Ok(()) },
                    _ = tokio::time::sleep(POLL) => {}
                }
                continue;
            };
            publish(&ctx, transfer.record(KIND_UPLINK)).await;
            let outcome = self.transfer(&ctx, &transfer).await;
            if matches!(outcome, Err(Stop::LinkLost)) {
                self.shared.state().paused = true;
            }
            let ended = self.update(&transfer.id, |t| match outcome {
                Ok(()) => t.state = "finished",
                Err(Stop::Canceled) => t.state = "canceled",
                Err(Stop::Failed(reason)) => (t.state, t.reason) = ("failed", Some(reason)),
                Err(Stop::LinkLost) => (t.state, t.reason) = ("failed", Some("the link is not connected".into())),
            });
            // The staged copy is of no use once the transfer has ended.
            let _ = std::fs::remove_file(self.options.uplink_dir().join(&ended.name));
            publish(&ctx, ended.record(KIND_UPLINK)).await;
        }
    }
}

impl UplinkHandler {
    /// Change the transfer in the shared list and return its new value.
    fn update(&self, id: &str, change: impl FnOnce(&mut Transfer)) -> Transfer {
        let mut state = self.shared.state();
        let transfer = state.uplinks.iter_mut().find(|t| t.id == id).expect("an active uplink stays in the list");
        change(transfer);
        transfer.clone()
    }

    /// Send one packet. No answer within the time-out is no progress (FIL-5).
    async fn send(&self, ctx: &Context, seq: &mut u32, file_packet: FilePacket) -> Result<(), Stop> {
        let bytes = packet::encode(packet::descriptor_bytes(&ctx.dictionary), *seq, &file_packet);
        *seq = seq.wrapping_add(1);
        let meta = PacketMeta::default();
        let sending = ctx.uplink.send_packet(&bytes, &meta, "files", false);
        match tokio::time::timeout(self.options.timeout, sending).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(SendError::LinkDown)) => Err(Stop::LinkLost),
            Ok(Err(e)) => Err(Stop::Failed(e.to_string())),
            Err(_) => Err(Stop::Failed(format!("no progress for {} s", self.options.timeout.as_secs_f64()))),
        }
    }

    /// Start packet, data packets, end packet: the sequence of the Python GDS.
    async fn transfer(&self, ctx: &Context, transfer: &Transfer) -> Result<(), Stop> {
        let path = self.options.uplink_dir().join(&transfer.name);
        let mut file = std::fs::File::open(&path).map_err(|e| Stop::Failed(format!("{}: {e}", transfer.name)))?;
        let mut seq = 0;
        let start = FilePacket::Start {
            size: transfer.size as u32,
            source: transfer.source.clone().into_bytes(),
            destination: transfer.destination.clone().into_bytes(),
        };
        self.send(ctx, &mut seq, start).await?;
        let (mut checksum, mut offset, mut percent) = (Checksum::default(), 0u64, 0);
        let mut chunk = vec![0; self.options.chunk];
        loop {
            tokio::time::sleep(self.options.cooldown).await;
            let (cancel, paused) = {
                let state = self.shared.state();
                (state.uplinks.iter().any(|t| t.id == transfer.id && t.cancel), state.paused)
            };
            if ctx.status.link() != "connected" {
                return Err(Stop::LinkLost);
            }
            // Pausing the queue cancels the file being sent, as the Python GDS does.
            if cancel || paused || ctx.shutdown.is_cancelled() {
                self.send(ctx, &mut seq, FilePacket::Cancel).await?;
                return Err(Stop::Canceled);
            }
            let length = read_chunk(&mut file, &mut chunk).map_err(|e| Stop::Failed(e.to_string()))?;
            // The size is in the start packet: never send more, even if the file grew.
            let length = length.min((transfer.size - offset) as usize);
            if length == 0 {
                return self.send(ctx, &mut seq, FilePacket::End { checksum: checksum.0 }).await;
            }
            checksum.update(&chunk[..length], offset);
            self.send(ctx, &mut seq, FilePacket::Data { offset: offset as u32, data: chunk[..length].to_vec() })
                .await?;
            offset += length as u64;
            let progress = self.update(&transfer.id, |t| t.done = offset);
            // One record per percent, not one per packet.
            let now = offset * 100 / transfer.size.max(1);
            if now != percent {
                percent = now;
                publish(ctx, progress.record(KIND_UPLINK)).await;
            }
        }
    }
}

/// Fill `chunk` unless the file ends first: every data packet but the last is full.
fn read_chunk(file: &mut std::fs::File, chunk: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < chunk.len() {
        match file.read(&mut chunk[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}
