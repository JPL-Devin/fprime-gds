//! F Prime file packets (`Fw::FilePacket`) and the CFDP checksum of a file.
//!
//! After the packet type field: type (U8), sequence index (U32), then
//! start: file size (U32), source path and destination path (each U8 length, bytes);
//! data: offset (U32), length (U16), bytes; end: checksum (U32); cancel: nothing.

use fgds_core::dictionary::{Dictionary, Reader, Type};
use fgds_core::PacketType;

#[derive(Debug, Clone, PartialEq)]
pub enum FilePacket {
    Start { size: u32, source: Vec<u8>, destination: Vec<u8> },
    Data { offset: u32, data: Vec<u8> },
    End { checksum: u32 },
    Cancel,
}

impl FilePacket {
    pub fn name(&self) -> &'static str {
        match self {
            FilePacket::Start { .. } => "start",
            FilePacket::Data { .. } => "data",
            FilePacket::End { .. } => "end",
            FilePacket::Cancel => "cancel",
        }
    }
}

/// Bytes of the packet type field (`FwPacketDescriptorType`).
pub fn descriptor_bytes(dict: &Dictionary) -> usize {
    match dict.config_type("FwPacketDescriptorType", Type::U16) {
        Type::Int { bits, .. } => usize::from(bits / 8),
        _ => 2,
    }
}

/// The whole packet, packet type field first.
pub fn encode(descriptor_bytes: usize, seq: u32, packet: &FilePacket) -> Vec<u8> {
    let descriptor = u64::from(PacketType::FILE.0).to_be_bytes();
    let mut out = descriptor[8 - descriptor_bytes.min(8)..].to_vec();
    let kind = match packet {
        FilePacket::Start { .. } => 0u8,
        FilePacket::Data { .. } => 1,
        FilePacket::End { .. } => 2,
        FilePacket::Cancel => 3,
    };
    out.push(kind);
    out.extend_from_slice(&seq.to_be_bytes());
    match packet {
        FilePacket::Start { size, source, destination } => {
            out.extend_from_slice(&size.to_be_bytes());
            for path in [source, destination] {
                // The length is one byte; callers give at most 255 bytes.
                let path = &path[..path.len().min(255)];
                out.push(path.len() as u8);
                out.extend_from_slice(path);
            }
        }
        FilePacket::Data { offset, data } => {
            let data = &data[..data.len().min(usize::from(u16::MAX))];
            out.extend_from_slice(&offset.to_be_bytes());
            out.extend_from_slice(&(data.len() as u16).to_be_bytes());
            out.extend_from_slice(data);
        }
        FilePacket::End { checksum } => out.extend_from_slice(&checksum.to_be_bytes()),
        FilePacket::Cancel => {}
    }
    out
}

/// Decode what follows the packet type field. Every byte must be accounted for.
pub fn decode(body: &[u8]) -> Result<(u32, FilePacket), String> {
    let short = |e: fgds_core::dictionary::TypeError| format!("truncated file packet: {}", e.0);
    let mut r = Reader::new(body);
    let kind = r.uint(8).map_err(short)?;
    let seq = r.uint(32).map_err(short)? as u32;
    let packet = match kind {
        0 => {
            let size = r.uint(32).map_err(short)? as u32;
            let mut path = || {
                let length = r.uint(8)? as usize;
                r.take(length).map(<[u8]>::to_vec)
            };
            let source = path().map_err(short)?;
            let destination = path().map_err(short)?;
            FilePacket::Start { size, source, destination }
        }
        1 => {
            let offset = r.uint(32).map_err(short)? as u32;
            let length = r.uint(16).map_err(short)? as usize;
            if length != r.remaining() {
                return Err(format!("file data packet says {length} bytes and holds {}", r.remaining()));
            }
            FilePacket::Data { offset, data: r.take(length).map_err(short)?.to_vec() }
        }
        2 => FilePacket::End { checksum: r.uint(32).map_err(short)? as u32 },
        3 => FilePacket::Cancel,
        other => return Err(format!("unknown file packet type {other}")),
    };
    if r.remaining() > 0 {
        return Err(format!("{} bytes after the end of a file {} packet", r.remaining(), packet.name()));
    }
    Ok((seq, packet))
}

/// The CFDP checksum F Prime uses for files: the sum of the big-endian 32-bit words of
/// the file, the last one padded with zeros. Data may be added in any order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Checksum(pub u32);

impl Checksum {
    pub fn update(&mut self, data: &[u8], offset: u64) {
        for (i, byte) in data.iter().enumerate() {
            let shift = 8 * (3 - (offset + i as u64) % 4);
            self.0 = self.0.wrapping_add(u32::from(*byte) << shift);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip_in_the_fprime_layout() {
        let start = FilePacket::Start { size: 5, source: b"a.bin".to_vec(), destination: b"/b".to_vec() };
        let bytes = encode(2, 0, &start);
        assert_eq!(bytes, [&[0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5][..], b"a.bin", &[2], b"/b"].concat());
        assert_eq!(decode(&bytes[2..]), Ok((0, start)));
        let data = FilePacket::Data { offset: 4, data: vec![9, 8] };
        assert_eq!(encode(2, 1, &data), [0, 3, 1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 2, 9, 8]);
        assert_eq!(decode(&encode(2, 1, &data)[2..]), Ok((1, data)));
        assert_eq!(encode(1, 2, &FilePacket::End { checksum: 0x01020304 }), [3, 2, 0, 0, 0, 2, 1, 2, 3, 4]);
        assert_eq!(decode(&[3, 0, 0, 0, 7]), Ok((7, FilePacket::Cancel)));
    }

    #[test]
    fn every_truncation_and_every_type_byte_is_an_error_or_a_packet() {
        let start = FilePacket::Start { size: 5, source: b"a.bin".to_vec(), destination: b"/b".to_vec() };
        let data = FilePacket::Data { offset: 4, data: vec![9, 8, 7] };
        for packet in [start, data, FilePacket::End { checksum: 1 }, FilePacket::Cancel] {
            let bytes = encode(0, 3, &packet);
            for cut in 0..bytes.len() {
                assert!(decode(&bytes[..cut]).is_err(), "{} cut at {cut}", packet.name());
            }
            let mut longer = bytes.clone();
            longer.push(0);
            assert!(decode(&longer).is_err());
            for kind in 0..=255u8 {
                let mut changed = bytes.clone();
                changed[0] = kind;
                let _ = decode(&changed);
            }
        }
        assert_eq!(decode(&[9, 0, 0, 0, 0]), Err("unknown file packet type 9".into()));
    }

    #[test]
    fn checksum_is_the_sum_of_big_endian_words_in_any_order() {
        let mut whole = Checksum::default();
        whole.update(&[1, 2, 3, 4, 5, 6], 0);
        assert_eq!(whole.0, 0x01020304 + 0x05060000);
        let mut parts = Checksum::default();
        parts.update(&[4, 5, 6], 3);
        parts.update(&[1, 2, 3], 0);
        assert_eq!(parts, whole);
        let mut wraps = Checksum::default();
        wraps.update(&[0xff; 8], 0);
        assert_eq!(wraps.0, 0xffff_fffe);
    }
}
