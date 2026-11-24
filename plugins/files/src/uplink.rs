//! The uplink queue (FIL-1, FIL-4, FIL-5): one file at a time, as file packets.

use crate::packet::{self, Checksum, FilePacket};
use crate::{publish, Options, Shared, Transfer, KIND_UPLINK};
use fgds_core::{async_trait, Context, Filter, Handler, PacketMeta, Record, RecordStream, Result, SendError};
use std::io::{Read, Seek};
use std::sync::Arc;
use std::time::Duration;

/// How often the link state is looked at when nothing else happens.
const POLL: Duration = Duration::from_millis(100);

/// How long after the end packet the flight side is watched for its answer; the next file of the queue
/// is sent meanwhile. `FileReceived` of `Svc.FileUplink` for the destination ends the wait as `finished`,
/// a refusal as `failed`; without either the transfer ends as `sent`: nothing says the file arrived.
const CONFIRM: Duration = Duration::from_secs(2);

/// The warnings of `Svc.FileUplink` that say a file is not received as sent.
const REFUSALS: [&str; 7] = [
    "BadChecksum",
    "FileOpenError",
    "FileWriteError",
    "InvalidReceiveMode",
    "PacketOutOfBounds",
    "PacketOutOfOrder",
    "DecodeError",
];

/// What an event says about the transfer in progress.
#[derive(Debug, PartialEq)]
pub(crate) enum Seen {
    Received,
    Failed(String),
}

/// What an event record says about the uplink to `destination`. A refusal that names a file counts for
/// that file only; one that names none counts for the transfer the flight software is receiving.
pub(crate) fn seen(record: &Record, destination: &str) -> Option<Seen> {
    if record.kind != "fprime.event" {
        return None;
    }
    let name = record.body["name"].as_str().unwrap_or_default();
    let text = record.body["display_text"].as_str().unwrap_or_default();
    let reason = || Seen::Failed(format!("the flight software reported {name}: {text}"));
    if record.body["severity"] == "FATAL" {
        return Some(reason());
    }
    let short = name.rsplit('.').next().unwrap_or_default();
    let args = record.body["args"].as_object();
    let file = args.and_then(|args| args.get("fileName")).and_then(|v| v.as_str());
    if short == "FileReceived" {
        return file.is_some_and(|file| names(file, destination)).then_some(Seen::Received);
    }
    let warning = record.body["severity"] == "WARNING_HI" && REFUSALS.contains(&short);
    (warning && file.is_none_or(|file| names(file, destination))).then(reason)
}

/// The shortest file name of an event that is taken as the start of a longer destination: the flight
/// software cuts the name to the size of the event string (40 bytes in stock F Prime), possibly inside
/// a character, which then reads as U+FFFD.
const CUT: usize = 36;

/// Whether the file name of an event is `destination`, whole or cut by the flight software.
fn names(file: &str, destination: &str) -> bool {
    let kept = file.trim_end_matches('\u{fffd}');
    file == destination || (kept.len() >= CUT && destination.starts_with(kept))
}

pub(crate) struct UplinkHandler {
    pub shared: Arc<Shared>,
    pub options: Options,
    /// Transfers whose end packet is sent and whose outcome is not known yet.
    pub unconfirmed: Vec<Unconfirmed>,
}

pub(crate) struct Unconfirmed {
    id: String,
    destination: String,
    until: tokio::time::Instant,
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
        // Events tell how the flight side took a transfer; its own records are read and dropped.
        Filter::kinds(&[KIND_UPLINK, "fprime.event"])
    }

    async fn run(mut self: Box<Self>, ctx: Context, mut records: RecordStream) -> Result<()> {
        let mut shutdown = ctx.shutdown.clone();
        // The queue runs from the start, as the Python GDS queue does. It pauses when a link that
        // was connected is lost (FIL-4) or when the operator asks; before the first connection
        // files wait in the queue.
        let mut was_connected = false;
        loop {
            let connected = ctx.status.link() == "connected";
            if was_connected && !connected {
                self.shared.state().paused = true;
            }
            was_connected = connected;
            let next = {
                let mut state = self.shared.state();
                let paused = state.paused || !connected;
                state.uplinks.iter_mut().find(|t| t.state == "queued" && !paused).map(|t| {
                    t.state = "transferring";
                    t.started = Transfer::now();
                    t.clone()
                })
            };
            // Records are read also when idle so that the subscription never lags.
            while let Ok(Some(record)) = tokio::time::timeout(Duration::ZERO, records.next()).await {
                self.settle(&ctx, Some(&record)).await;
            }
            self.settle(&ctx, None).await;
            let Some(transfer) = next else {
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = self.shared.wake.notified() => {}
                    record = records.next() => match record {
                        Some(record) => self.settle(&ctx, Some(&record)).await,
                        None => return Ok(()),
                    },
                    _ = tokio::time::sleep(POLL) => {}
                }
                continue;
            };
            publish(&ctx, transfer.record(KIND_UPLINK)).await;
            let outcome = self.transfer(&ctx, &transfer, &mut records).await;
            if matches!(outcome, Err(Stop::LinkLost)) {
                self.shared.state().paused = true;
            }
            was_connected = ctx.status.link() == "connected";
            let ended = self.update(&transfer.id, |t| {
                // As the Python GDS: the end time is when the end packet is sent.
                t.ended = Transfer::now();
                match outcome {
                    Ok(()) => {}
                    Err(Stop::Canceled) => t.state = "canceled",
                    Err(Stop::Failed(reason)) => (t.state, t.reason) = ("failed", Some(reason)),
                    Err(Stop::LinkLost) => (t.state, t.reason) = ("failed", Some("the link is not connected".into())),
                }
            });
            // The staged copy is of no use once the transfer has ended.
            let _ = std::fs::remove_file(self.options.uplink_dir().join(&ended.id));
            publish(&ctx, ended.record(KIND_UPLINK)).await;
            if !ended.ended() {
                let until = tokio::time::Instant::now() + CONFIRM;
                self.unconfirmed.push(Unconfirmed { id: ended.id, destination: ended.destination, until });
            }
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

    /// After an end packet: finished on `FileReceived`; failed when the link goes away or the flight
    /// software refuses the file; `sent` when `CONFIRM` has passed without either.
    async fn settle(&mut self, ctx: &Context, record: Option<&Record>) {
        let connected = ctx.status.link() == "connected";
        let now = tokio::time::Instant::now();
        let mut waiting = Vec::new();
        for item in std::mem::take(&mut self.unconfirmed) {
            let outcome = match record.and_then(|r| seen(r, &item.destination)) {
                _ if !connected => Some(Err(Some("the link is not connected".to_string()))),
                Some(Seen::Received) => Some(Ok(())),
                Some(Seen::Failed(reason)) => Some(Err(Some(reason))),
                None if now >= item.until => Some(Err(None)),
                None => None,
            };
            let Some(outcome) = outcome else {
                waiting.push(item);
                continue;
            };
            let ended = self.update(&item.id, |t| match outcome {
                Ok(()) => t.state = "finished",
                Err(Some(reason)) => (t.state, t.reason) = ("failed", Some(reason)),
                Err(None) => {
                    t.state = "sent";
                    t.reason = Some("every packet is sent; the flight software did not confirm the file".into());
                }
            });
            publish(ctx, ended.record(KIND_UPLINK)).await;
        }
        self.unconfirmed = waiting;
    }

    /// Start packet, data packets, end packet: the sequence of the Python GDS.
    async fn transfer(&mut self, ctx: &Context, transfer: &Transfer, records: &mut RecordStream) -> Result<(), Stop> {
        let path = self.options.uplink_dir().join(&transfer.id);
        let mut file = std::fs::File::open(&path).map_err(|e| Stop::Failed(format!("{}: {e}", transfer.name)))?;
        let mut seq = 0;
        let start = FilePacket::Start {
            size: transfer.size as u32,
            source: transfer.source.clone().into_bytes(),
            destination: transfer.destination.clone().into_bytes(),
        };
        self.send(ctx, &mut seq, start).await?;
        let (mut checksum, mut offset, mut percent) = (Checksum::default(), 0u64, 0);
        let size = transfer.chunk.unwrap_or(self.options.chunk);
        let cooldown = transfer.cooldown.unwrap_or(self.options.cooldown);
        let mut chunk = vec![0; size];
        // With a list of packets (`Select Packets`), as `TransmitFile.read` of the Python GDS: packet `n` is the
        // chunk at `(n - 1) * chunk size`; the file ends after the last of the list, or at the first one that
        // lies beyond the end of the file.
        let mut listed = transfer.packets.as_ref().map(|list| list.iter());
        let mut sent = 0u64;
        loop {
            tokio::time::sleep(cooldown).await;
            let (cancel, paused) = {
                let state = self.shared.state();
                (state.uplinks.iter().any(|t| t.id == transfer.id && t.cancel), state.paused)
            };
            if ctx.status.link() != "connected" {
                return Err(Stop::LinkLost);
            }
            while let Ok(Some(record)) = tokio::time::timeout(Duration::ZERO, records.next()).await {
                self.settle(ctx, Some(&record)).await;
                if let Some(Seen::Failed(reason)) = seen(&record, &transfer.destination) {
                    return Err(Stop::Failed(reason));
                }
            }
            // Pausing the queue cancels the file being sent, as the Python GDS does.
            if cancel || paused || ctx.shutdown.is_cancelled() {
                self.send(ctx, &mut seq, FilePacket::Cancel).await?;
                return Err(Stop::Canceled);
            }
            let length = match listed.as_mut().map(Iterator::next) {
                Some(None) => 0,
                Some(Some(number)) => {
                    offset = (number - 1).saturating_mul(size as u64);
                    file.seek(std::io::SeekFrom::Start(offset)).map_err(|e| Stop::Failed(e.to_string()))?;
                    read_chunk(&mut file, &mut chunk).map_err(|e| Stop::Failed(e.to_string()))?
                }
                None => read_chunk(&mut file, &mut chunk).map_err(|e| Stop::Failed(e.to_string()))?,
            };
            // The size is in the start packet: never send more, even if the file grew.
            let length = length.min(transfer.size.saturating_sub(offset) as usize);
            if length == 0 {
                self.send(ctx, &mut seq, FilePacket::End { checksum: checksum.0 }).await?;
                return Ok(());
            }
            checksum.update(&chunk[..length], offset);
            self.send(ctx, &mut seq, FilePacket::Data { offset: offset as u32, data: chunk[..length].to_vec() })
                .await?;
            offset += length as u64;
            sent += 1;
            // Of a list of packets, the part of the list that is sent, as the Python GDS.
            let done = match &transfer.packets {
                Some(list) => transfer.size * sent / list.len() as u64,
                None => offset,
            };
            let progress = self.update(&transfer.id, |t| t.done = done);
            // One record per percent, not one per packet.
            let now = done * 100 / transfer.size.max(1);
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

#[cfg(test)]
mod tests {
    use super::{seen, Seen};
    use fgds_core::serde_json::{json, Value};
    use fgds_core::{Record, Timestamp};

    fn event(name: &str, severity: &str, args: Value) -> Record {
        let body = json!({"name": name, "severity": severity, "display_text": "text", "args": args});
        Record {
            seq: 1,
            run: 1,
            ground_time: Timestamp(0),
            kind: "fprime.event".into(),
            flight_time: None,
            id: None,
            entity: None,
            stored: true,
            raw: Default::default(),
            meta: Default::default(),
            body,
        }
    }

    #[test]
    fn only_the_event_of_the_flight_software_ends_an_uplink() {
        let long = format!("n9-ab{}.bin", "\u{e9}".repeat(30));
        assert!(super::names(&format!("n9-ab{}\u{fffd}", "\u{e9}".repeat(17)), &long));
        assert!(!super::names("n9", &long));
        assert!(!super::names("other.bin", "a.bin") && super::names("a.bin", "a.bin"));
        let to = "/dir/up.bin";
        let received = event("f.fileUplink.FileReceived", "ACTIVITY_HI", json!({"fileName": to}));
        assert_eq!(seen(&received, to), Some(Seen::Received));
        assert_eq!(seen(&received, "/other.bin"), None);
        // Stock Ref for a destination it cannot open: the transfer has failed, it is not finished.
        let refused = event("f.fileUplink.FileOpenError", "WARNING_HI", json!({"fileName": to}));
        assert!(matches!(seen(&refused, to), Some(Seen::Failed(reason)) if reason.contains("FileOpenError")));
        assert_eq!(seen(&refused, "/other.bin"), None);
        let mode = event("f.fileUplink.InvalidReceiveMode", "WARNING_HI", json!({"packetType": 1, "mode": 0}));
        assert!(matches!(seen(&mode, to), Some(Seen::Failed(_))));
        // A warning of another component says nothing about the file.
        assert_eq!(seen(&event("f.cmdDisp.OpCodeError", "WARNING_HI", json!({})), to), None);
        assert!(matches!(seen(&event("f.x.Stop", "FATAL", json!({})), to), Some(Seen::Failed(_))));
    }
}
