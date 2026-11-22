//! The framer/deframer stages of the Python GDS (`fprime_gds.common.communication.ccsds`),
//! one type per Python class, with the same results call for call. The layers in
//! `lib.rs` are built from them; `tests/vectors.rs` checks them against the vectors
//! generated from the Python tests.

use std::collections::HashMap;

/// The result of one `deframe` call.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// The packet found, `None` when the input holds no whole one.
    pub packet: Option<Vec<u8>>,
    /// Bytes of the input used up; what follows is kept for the next call.
    pub consumed: usize,
    /// Bytes thrown away, part of `consumed`.
    pub discarded: Vec<u8>,
}

pub trait Stage: Send {
    fn frame(&mut self, data: &[u8]) -> Result<Vec<u8>, String>;
    /// Look for one packet at the front of `data`. `warnings` receives the messages
    /// the Python implementation logs.
    fn deframe(&mut self, data: &[u8], warnings: &mut Vec<String>) -> Step;
}

/// Call `deframe` until no packet is found: (packets, bytes kept, bytes discarded).
pub fn deframe_all(stage: &mut dyn Stage, data: &[u8], warnings: &mut Vec<String>) -> (Vec<Vec<u8>>, Vec<u8>, Vec<u8>) {
    let (mut packets, mut discarded, mut at) = (Vec::new(), Vec::new(), 0);
    loop {
        let step = stage.deframe(&data[at..], warnings);
        at += step.consumed;
        discarded.extend_from_slice(&step.discarded);
        match step.packet {
            Some(packet) => packets.push(packet),
            None => return (packets, data[at..].to_vec(), discarded),
        }
    }
}

pub const SPACE_PACKET_HEADER: usize = 6;
pub const IDLE_APID: u16 = 0x7FF;
const SEQUENCE_COUNT_MAXIMUM: u32 = 1 << 14;

/// The fields of a Space Packet primary header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpacePacketHeader {
    pub version: u8,
    /// True for telecommand, false for telemetry.
    pub telecommand: bool,
    pub apid: u16,
    pub seq_count: u16,
    /// Whole packet, header included.
    pub packet_len: usize,
}

impl SpacePacketHeader {
    /// `data` holds at least the six header bytes.
    pub fn parse(data: &[u8]) -> SpacePacketHeader {
        SpacePacketHeader {
            version: data[0] >> 5,
            telecommand: data[0] & 0x10 != 0,
            apid: u16::from_be_bytes([data[0] & 0x07, data[1]]),
            seq_count: u16::from_be_bytes([data[2] & 0x3F, data[3]]),
            packet_len: SPACE_PACKET_HEADER + usize::from(u16::from_be_bytes([data[4], data[5]])) + 1,
        }
    }

    fn is_telemetry(&self) -> bool {
        self.version == 0 && !self.telecommand
    }
}

/// The application identifiers of the deployment: the `ComCfg.Apid` enumeration,
/// whose value is the first field of every F Prime packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apids {
    /// Size of the field in bytes.
    pub width: usize,
    /// The constants of the enumeration; `None` accepts every value.
    pub values: Option<Vec<u64>>,
}

impl Default for Apids {
    fn default() -> Self {
        Apids { width: 2, values: None }
    }
}

/// The next sequence count of each application identifier.
pub type SequenceCounts = std::sync::Arc<std::sync::Mutex<HashMap<u16, u32>>>;

/// `SpacePacketFramerDeframer`: wraps a packet in a telecommand Space Packet; takes the
/// payload out of exactly one whole telemetry Space Packet.
#[derive(Debug, Default)]
pub struct SpacePacket {
    apids: Apids,
    sequence_counts: SequenceCounts,
    /// Header of the packet the last `deframe` returned.
    pub last: Option<SpacePacketHeader>,
}

impl SpacePacket {
    pub fn new(apids: Apids) -> Self {
        SpacePacket { apids, ..Default::default() }
    }

    /// A stage that counts with others: the Python GDS has one `SpacePacketFramerDeframer` for both
    /// directions, so a packet received on an identifier advances the count of the next one sent on it
    /// (`Ref`'s `ApidManager` expects that).
    pub fn sharing(apids: Apids, sequence_counts: SequenceCounts) -> Self {
        SpacePacket { apids, sequence_counts, last: None }
    }

    /// The count expected next for `apid`, which then advances.
    fn sequence_count(&mut self, apid: u16) -> u32 {
        let mut counts = self.sequence_counts.lock().unwrap_or_else(|e| e.into_inner());
        let count = counts.entry(apid).or_insert(0);
        let current = *count;
        *count = (current + 1) % SEQUENCE_COUNT_MAXIMUM;
        current
    }
}

impl Stage for SpacePacket {
    fn frame(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let width = self.apids.width;
        if data.len() < width || width > 8 {
            return Err(format!("packet of {} bytes has no {width}-byte packet type", data.len()));
        }
        let value = data[..width].iter().fold(0u64, |v, b| v << 8 | u64::from(*b));
        if self.apids.values.as_ref().is_some_and(|values| !values.contains(&value)) {
            return Err(format!("Value {value} out of range!"));
        }
        let apid = u16::try_from(value).ok().filter(|apid| *apid <= IDLE_APID);
        let apid = apid.ok_or(format!("application identifier {value} exceeds 11 bits"))?;
        let length = u16::try_from(data.len() - 1).map_err(|_| "packet too long for a Space Packet".to_string())?;
        let sequence = self.sequence_count(apid) as u16;
        let mut packet = Vec::with_capacity(SPACE_PACKET_HEADER + data.len());
        // Version 0, telecommand, no secondary header; unsegmented user data.
        packet.extend_from_slice(&(0x1000 | apid).to_be_bytes());
        packet.extend_from_slice(&(0xC000 | sequence).to_be_bytes());
        packet.extend_from_slice(&length.to_be_bytes());
        packet.extend_from_slice(data);
        Ok(packet)
    }

    fn deframe(&mut self, data: &[u8], warnings: &mut Vec<String>) -> Step {
        self.last = None;
        let rejected = || Step { packet: None, consumed: data.len(), discarded: data.to_vec() };
        if data.len() < SPACE_PACKET_HEADER {
            return rejected();
        }
        let header = SpacePacketHeader::parse(data);
        if !header.is_telemetry() || data.len() != header.packet_len {
            return rejected();
        }
        // A gap is reported, not dropped.
        let expected = self.sequence_count(header.apid);
        if u32::from(header.seq_count) != expected {
            warnings.push(format!(
                "APID {} received sequence count: {} (expected: {expected})",
                header.apid, header.seq_count
            ));
            let mut counts = self.sequence_counts.lock().unwrap_or_else(|e| e.into_inner());
            counts.insert(header.apid, u32::from(header.seq_count) + 1);
        }
        self.last = Some(header);
        Step { packet: Some(data[SPACE_PACKET_HEADER..].to_vec()), consumed: data.len(), discarded: Vec::new() }
    }
}

/// `SpacePacketSplitterFramerDeframer`: cuts header-aligned bytes into whole telemetry
/// Space Packets (header kept), dropping idle packets. Framing passes data through.
#[derive(Debug, Default)]
pub struct SpacePacketSplitter;

impl Stage for SpacePacketSplitter {
    fn frame(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }

    fn deframe(&mut self, data: &[u8], _: &mut Vec<String>) -> Step {
        let mut step = Step::default();
        while data.len() - step.consumed >= SPACE_PACKET_HEADER {
            let rest = &data[step.consumed..];
            let header = SpacePacketHeader::parse(rest);
            if !header.is_telemetry() {
                step.discarded.push(rest[0]);
                step.consumed += 1;
                continue;
            }
            if rest.len() < header.packet_len {
                break;
            }
            step.consumed += header.packet_len;
            if header.apid != IDLE_APID {
                step.packet = Some(rest[..header.packet_len].to_vec());
                break;
            }
        }
        step
    }
}

const TC_HEADER: usize = 5;
pub const TM_HEADER: usize = 6;
pub const TM_TRAILER: usize = 2;
const FHP_MASK: u16 = 0x7FF;
/// First header pointer: no packet starts in this frame.
const FHP_NO_PACKET_START: u16 = 0x7FF;
/// First header pointer: the frame holds idle data only.
const FHP_IDLE_DATA_ONLY: u16 = 0x7FE;
const MAX_PACKET_SIZE: usize = SPACE_PACKET_HEADER + 65536;
const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_3740);

pub const FALLBACK_SCID: u16 = 0x44;
pub const FALLBACK_FRAME_SIZE: usize = 1024;
pub const DEFAULT_VCID: u8 = 1;

/// `SpaceDataLinkFramerDeframer`: wraps data in a TC frame; takes the whole Space
/// Packets out of fixed-size TM frames, joining packets that span frames.
#[derive(Debug)]
pub struct SpaceDataLink {
    pub scid: u16,
    pub vcid: u8,
    pub frame_size: usize,
    /// Start of a packet that continues in the next frame.
    pending: Vec<u8>,
    last_vc_count: Option<u8>,
    /// True when the last `deframe` discarded bytes because a checksum did not match.
    pub checksum_failed: bool,
}

impl SpaceDataLink {
    pub fn new(scid: u16, vcid: u8, frame_size: usize) -> Self {
        SpaceDataLink { scid, vcid, frame_size, pending: Vec::new(), last_vc_count: None, checksum_failed: false }
    }

    /// The whole Space Packets available after this frame's data field.
    fn reassemble(
        &mut self,
        field: &[u8],
        first_header_pointer: u16,
        vc_count: u8,
        warnings: &mut Vec<String>,
    ) -> Vec<u8> {
        if self.last_vc_count.is_some_and(|last| vc_count != last.wrapping_add(1)) && !self.pending.is_empty() {
            warnings.push("[WARNING] TM frame loss detected. Discarding partial packet data.".into());
            self.pending.clear();
        }
        self.last_vc_count = Some(vc_count);
        if first_header_pointer == FHP_IDLE_DATA_ONLY {
            self.pending.clear();
            return Vec::new();
        }
        if first_header_pointer == FHP_NO_PACKET_START {
            if !self.pending.is_empty() {
                self.pending.extend_from_slice(field);
                if self.pending.len() > MAX_PACKET_SIZE {
                    warnings.push(
                        "[WARNING] Spanned packet exceeds maximum packet size. Discarding partial packet data.".into(),
                    );
                    self.pending.clear();
                }
            }
            return Vec::new();
        }
        let first = usize::from(first_header_pointer);
        if first >= field.len() {
            warnings.push(
                "[WARNING] First Header Pointer beyond TM frame data field. Discarding frame and partial packet data."
                    .into(),
            );
            self.pending.clear();
            return Vec::new();
        }
        let mut emitted = Vec::new();
        if !self.pending.is_empty() {
            emitted = std::mem::take(&mut self.pending);
            emitted.extend_from_slice(&field[..first]);
            if complete_packets(&emitted) != emitted.len() {
                warnings.push(
                    "[WARNING] First Header Pointer inconsistent with pending packet length. Discarding partial packet data."
                        .into(),
                );
                emitted.clear();
            }
        }
        let rest = &field[first..];
        let complete = complete_packets(rest);
        emitted.extend_from_slice(&rest[..complete]);
        self.pending = rest[complete..].to_vec();
        emitted
    }
}

/// Length of the whole Space Packets at the front of header-aligned `data`.
fn complete_packets(data: &[u8]) -> usize {
    let mut offset = 0;
    while data.len() - offset >= SPACE_PACKET_HEADER {
        let length = SPACE_PACKET_HEADER + usize::from(u16::from_be_bytes([data[offset + 4], data[offset + 5]])) + 1;
        if data.len() - offset < length {
            break;
        }
        offset += length;
    }
    offset
}

impl Stage for SpaceDataLink {
    fn frame(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let length = data.len() + TC_HEADER + TM_TRAILER - 1;
        if length >= 0x3FF {
            return Err("Length too-large for CCSDS format".into());
        }
        let mut frame = Vec::with_capacity(length + 1);
        // Version 0, bypass flag set, data frame; then the virtual channel and the length.
        frame.extend_from_slice(&(0x2000 | (self.scid & 0x3FF)).to_be_bytes());
        frame.extend_from_slice(&((u16::from(self.vcid & 0x3F) << 10) | length as u16).to_be_bytes());
        frame.push(0);
        frame.extend_from_slice(data);
        frame.extend_from_slice(&CRC.checksum(&frame).to_be_bytes());
        Ok(frame)
    }

    fn deframe(&mut self, data: &[u8], warnings: &mut Vec<String>) -> Step {
        let mut step = Step::default();
        self.checksum_failed = false;
        if self.frame_size < TM_HEADER + TM_TRAILER {
            return step;
        }
        while data.len() - step.consumed >= self.frame_size {
            let frame = &data[step.consumed..step.consumed + self.frame_size];
            let ids = u16::from_be_bytes([frame[0], frame[1]]);
            if (ids & 0x3FF0) >> 4 != self.scid || (ids & 0x000E) >> 1 != u16::from(self.vcid) {
                step.discarded.push(frame[0]);
                step.consumed += 1;
                continue;
            }
            let crc_offset = self.frame_size - TM_TRAILER;
            if u16::from_be_bytes([frame[crc_offset], frame[crc_offset + 1]]) != CRC.checksum(&frame[..crc_offset]) {
                warnings.push("[WARNING] Checksum validation failed.".into());
                self.checksum_failed = true;
                step.discarded.push(frame[0]);
                step.consumed += 1;
                continue;
            }
            let first_header_pointer = u16::from_be_bytes([frame[4], frame[5]]) & FHP_MASK;
            step.consumed += self.frame_size;
            let packets = self.reassemble(&frame[TM_HEADER..crc_offset], first_header_pointer, frame[3], warnings);
            if !packets.is_empty() {
                step.packet = Some(packets);
                break;
            }
        }
        step
    }
}

/// `TmFrameAggregatorFramerDeframer`: finds fixed-size TM frames in a byte stream and
/// returns each whole, header and trailer included. Framing passes data through.
#[derive(Debug)]
pub struct TmFrameAggregator {
    pub frame_size: usize,
    /// Checked in the frame header when known.
    pub scid: Option<u16>,
}

impl TmFrameAggregator {
    /// True when `data` (two bytes or more) may be the start of a TM frame.
    fn is_frame_start(&self, data: &[u8]) -> bool {
        let frame_id = u16::from_be_bytes([data[0], data[1]]);
        if frame_id >> 14 != 0 || self.scid.is_some_and(|scid| (frame_id >> 4) & 0x3FF != scid) {
            return false;
        }
        if data.len() < TM_HEADER {
            return true;
        }
        // No synchronisation or packet order flag, segment length identifier 3.
        let status = u16::from_be_bytes([data[4], data[5]]);
        if status & 0x6000 != 0 || status & 0x1800 != 0x1800 {
            return false;
        }
        let first_header_pointer = status & FHP_MASK;
        let data_field = self.frame_size.saturating_sub(TM_HEADER + TM_TRAILER);
        usize::from(first_header_pointer) < data_field
            || first_header_pointer == FHP_IDLE_DATA_ONLY
            || first_header_pointer == FHP_NO_PACKET_START
    }
}

impl Stage for TmFrameAggregator {
    fn frame(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }

    fn deframe(&mut self, data: &[u8], _: &mut Vec<String>) -> Step {
        let mut step = Step::default();
        while data.len() - step.consumed >= 2 {
            let rest = &data[step.consumed..];
            if !self.is_frame_start(rest) {
                step.discarded.push(rest[0]);
                step.consumed += 1;
                continue;
            }
            if rest.len() >= self.frame_size {
                step.packet = Some(rest[..self.frame_size].to_vec());
                step.consumed += self.frame_size;
            }
            break;
        }
        step
    }
}
