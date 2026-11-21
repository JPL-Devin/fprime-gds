//! File downlink (FIL-2, FIL-3): file packets into a file, with progress records.

use crate::packet::{self, Checksum, FilePacket};
use crate::{
    downlink_name, path_inside, publish, Options, Shared, State, Transfer, KIND_DOWNLINK, KIND_PACKET, KIND_PROBLEM,
};
use fgds_core::serde_json::json;
use fgds_core::{async_trait, Context, Filter, Handler, NewRecord, RecordStream, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use tokio::time::Instant;

/// The transfer in progress.
struct Active {
    transfer: Transfer,
    /// `None`: the file could not be created; its packets are discarded until the end.
    file: Option<File>,
    /// Sequence index of the next packet.
    expected: u32,
    /// Where the next data packet starts when none is lost.
    /// Where the next data packet is expected; `None` before the first one (`SendPartial` starts anywhere).
    seek: Option<u64>,
    last_packet: Instant,
}

pub(crate) struct DownlinkHandler {
    shared: Arc<Shared>,
    options: Options,
    active: Option<Active>,
}

#[async_trait]
impl Handler for DownlinkHandler {
    fn subscription(&self) -> Filter {
        Filter::kinds(&[KIND_PACKET])
    }

    async fn run(mut self: Box<Self>, ctx: Context, mut records: RecordStream) -> Result<()> {
        let mut shutdown = ctx.shutdown.clone();
        loop {
            let deadline = self.active.as_ref().map(|a| a.last_packet + self.options.timeout);
            let timed_out = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = timed_out => {
                    let reason = format!("no data for {} s", self.options.timeout.as_secs_f64());
                    self.end(&ctx, "failed", Some(reason)).await;
                }
                record = records.next() => match record {
                    Some(record) => self.packet(&ctx, &record.raw).await,
                    None => return Ok(()),
                },
            }
        }
    }
}

impl DownlinkHandler {
    pub fn new(shared: Arc<Shared>, options: Options) -> Self {
        DownlinkHandler { shared, options, active: None }
    }

    async fn packet(&mut self, ctx: &Context, raw: &[u8]) {
        // The decoder accepted these bytes; anything else is reported like any other problem.
        let (seq, file_packet) = match packet::decode(raw) {
            Ok(decoded) => decoded,
            Err(reason) => return self.problem(ctx, None, reason).await,
        };
        if let Some(active) = &mut self.active {
            active.last_packet = Instant::now();
        }
        let kind = file_packet.name();
        match file_packet {
            FilePacket::Start { size, source, destination } => self.start(ctx, seq, size, &source, &destination).await,
            FilePacket::Data { offset, data } => self.data(ctx, seq, offset, &data).await,
            FilePacket::End { checksum } => {
                self.check_seq(ctx, seq, kind).await;
                self.finish(ctx, seq, checksum).await;
            }
            FilePacket::Cancel => match self.active {
                Some(_) => self.end(ctx, "canceled", None).await,
                None => self.problem(ctx, Some(seq), "cancel packet without a transfer in progress".into()).await,
            },
        }
    }

    async fn start(&mut self, ctx: &Context, seq: u32, size: u32, source: &[u8], destination: &[u8]) {
        if self.active.is_some() {
            self.problem(ctx, Some(seq), "start packet during a transfer; that transfer ends as failed".into()).await;
            self.end(ctx, "failed", Some("another transfer started".into())).await;
        }
        let destination = String::from_utf8_lossy(destination).into_owned();
        let mut transfer = Transfer {
            id: self.shared.new_id("down"),
            name: String::new(),
            source: String::from_utf8_lossy(source).into_owned(),
            destination,
            size: u64::from(size),
            done: 0,
            state: "transferring",
            reason: None,
            problems: 0,
            cancel: false,
            started: Transfer::now(),
            ended: None,
        };
        let directory = self.options.downlink_dir();
        let created =
            downlink_name(&transfer.destination).and_then(|name| path_inside(&directory, &name)).and_then(|path| {
                transfer.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let open = || File::options().read(true).write(true).create(true).truncate(true).open(&path);
                std::fs::create_dir_all(&directory).and_then(|_| open()).map_err(|e| e.to_string())
            });
        let file = match created {
            Ok(file) => Some(file),
            Err(reason) => {
                (transfer.state, transfer.reason) = ("failed", Some(reason));
                transfer.ended = Transfer::now();
                None
            }
        };
        {
            let mut state = self.shared.state();
            state.downlinks.push(transfer.clone());
            State::trim(&mut state.downlinks);
        }
        publish(ctx, transfer.record(KIND_DOWNLINK)).await;
        self.active =
            Some(Active { transfer, file, expected: seq.wrapping_add(1), seek: None, last_packet: Instant::now() });
    }

    /// A sequence index that is not the next one: a lost, duplicated or reordered packet.
    async fn check_seq(&mut self, ctx: &Context, seq: u32, kind: &str) {
        let Some(active) = &mut self.active else { return };
        let expected = active.expected;
        active.expected = expected.max(seq.wrapping_add(1));
        if seq != expected {
            let what = if seq < expected { "duplicated or out of order" } else { "packets were lost" };
            self.problem(
                ctx,
                Some(seq),
                format!("{kind} packet has sequence index {seq}, expected {expected}: {what}"),
            )
            .await;
        }
    }

    async fn data(&mut self, ctx: &Context, seq: u32, offset: u32, data: &[u8]) {
        if self.active.is_none() {
            return self.problem(ctx, Some(seq), "data packet without a transfer in progress".into()).await;
        }
        self.check_seq(ctx, seq, "data").await;
        let Some(active) = &mut self.active else { return };
        let Some(file) = &mut active.file else { return };
        let (offset, end) = (u64::from(offset), u64::from(offset) + data.len() as u64);
        let problem = if end > active.transfer.size {
            Some(format!("data at offset {offset} ends after the file size {}; not written", active.transfer.size))
        } else if let Err(e) = file.seek(SeekFrom::Start(offset)).and_then(|_| file.write_all(data)) {
            Some(format!("write failed: {e}"))
        } else {
            let seek = active.seek.replace(end).unwrap_or(offset);
            active.transfer.done = active.transfer.done.max(end);
            (offset != seek).then(|| format!("data at offset {offset}, expected offset {seek}"))
        };
        if let Some(reason) = problem {
            self.problem(ctx, Some(seq), reason).await;
        }
        let Some(active) = &self.active else { return };
        let percent = |done: u64| done * 100 / active.transfer.size.max(1);
        let before = self.set(&active.transfer);
        // One record per percent, not one per packet.
        if percent(before) != percent(active.transfer.done) {
            publish(ctx, active.transfer.record(KIND_DOWNLINK)).await;
        }
    }

    /// The end packet: a file of the announced size is finished when its checksum is the one the
    /// packet gives.
    async fn finish(&mut self, ctx: &Context, seq: u32, checksum: u32) {
        let Some(active) = &mut self.active else {
            return self.problem(ctx, Some(seq), "end packet without a transfer in progress".into()).await;
        };
        let Some(file) = &mut active.file else {
            // Already reported as failed when the file could not be created.
            self.active = None;
            return;
        };
        let reason = match checksum_of(file) {
            Err(e) => Some(format!("the file could not be read back: {e}")),
            // Fewer bytes than the start packet announced is what `SendPartial` sends (the size is
            // that of the whole file): finished when the checksum agrees, as the Python GDS shows
            // it, with the bytes received as the progress.
            Ok((_, length)) if length > active.transfer.size => {
                Some(format!("received {length} of {} bytes", active.transfer.size))
            }
            Ok((found, _)) if found.0 != checksum => {
                Some(format!("checksum is {:#010x}, the end packet says {checksum:#010x}", found.0))
            }
            Ok(_) => None,
        };
        let state = if reason.is_some() { "failed" } else { "finished" };
        self.end(ctx, state, reason).await;
    }

    /// Store the transfer in the shared list; returns the bytes it had there.
    fn set(&self, transfer: &Transfer) -> u64 {
        let mut state = self.shared.state();
        match state.downlinks.iter_mut().find(|t| t.id == transfer.id) {
            Some(listed) => std::mem::replace(listed, transfer.clone()).done,
            None => 0,
        }
    }

    async fn end(&mut self, ctx: &Context, state: &'static str, reason: Option<String>) {
        let Some(mut active) = self.active.take() else { return };
        // A transfer whose file could not be created has failed already.
        if active.file.is_some() {
            (active.transfer.state, active.transfer.reason) = (state, reason);
            active.transfer.ended = Transfer::now();
            self.set(&active.transfer);
            publish(ctx, active.transfer.record(KIND_DOWNLINK)).await;
        }
    }

    async fn problem(&mut self, ctx: &Context, seq: Option<u32>, reason: String) {
        let (mut name, mut entity) = (None, None);
        if let Some(active) = &mut self.active {
            active.transfer.problems += 1;
            self.shared
                .state()
                .downlinks
                .iter_mut()
                .filter(|t| t.id == active.transfer.id)
                .for_each(|t| t.problems += 1);
            (name, entity) = (Some(active.transfer.name.clone()), Some(active.transfer.id.clone()));
        }
        ctx.status.count("file_packet_problems", 1);
        let body = json!({"reason": reason, "seq": seq, "name": name, "transfer": entity});
        publish(ctx, NewRecord::new(KIND_PROBLEM, body)).await;
    }
}

/// The CFDP checksum and the length of what was written.
fn checksum_of(file: &mut File) -> std::io::Result<(Checksum, u64)> {
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    let (mut checksum, mut offset, mut buffer) = (Checksum::default(), 0u64, vec![0; 64 * 1024]);
    loop {
        match file.read(&mut buffer)? {
            0 => return Ok((checksum, offset)),
            n => {
                checksum.update(&buffer[..n], offset);
                offset += n as u64;
            }
        }
    }
}
