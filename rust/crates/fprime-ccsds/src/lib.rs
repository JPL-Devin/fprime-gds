//! CCSDS framers/deframers for the F´ ground system.
//!
//! Mirrors `fprime_gds.common.communication.ccsds.*` byte-for-byte:
//!
//! * [`SpacePacketFramer`] / [`SpacePacketDeframer`] — CCSDS 133.0 Space
//!   Packet (the inner protocol).  Uplink builds TC space packets; downlink
//!   accepts TM space packets.  APID is taken from the leading 4 bytes of
//!   the F´ payload (the F´ packet descriptor / opcode header).
//! * [`TcFramer`] — CCSDS 232.0 TC Transfer Frame (5-byte header +
//!   payload + 2-byte CRC-16/CCITT) for uplink.  Bypass-FARM, Type-D.
//! * [`TmDeframer`] — CCSDS 132.0 TM Transfer Frame (fixed-size,
//!   6-byte header + payload + 2-byte CRC-16/CCITT) for downlink.
//! * [`ChainedFramer`] / [`ChainedDeframer`] — composes an outer transport
//!   (TC/TM) around an inner protocol (Space Packet).

#![deny(rust_2018_idioms)]

use fprime_frame::FrameError;
use thiserror::Error;

pub mod chain;
pub mod space_data_link;
pub mod space_packet;

pub use chain::{ChainedDeframer, ChainedFramer};
pub use space_data_link::{TcFramer, TmDeframer};
pub use space_packet::{SpacePacketDeframer, SpacePacketFramer};

#[derive(Debug, Error)]
pub enum CcsdsError {
    #[error("ccsds: payload empty (need APID prefix)")]
    Empty,
    #[error("ccsds: APID {0:#x} out of 11-bit range")]
    ApidOutOfRange(u32),
    #[error("ccsds: payload too large: {0}")]
    TooLarge(usize),
    #[error("ccsds: spacecraft id {0:#x} out of 10-bit range")]
    ScidOutOfRange(u32),
    #[error("ccsds: virtual channel id {0:#x} out of 6-bit range")]
    VcidOutOfRange(u32),
}

impl From<CcsdsError> for FrameError {
    fn from(err: CcsdsError) -> Self {
        FrameError::Ccsds(err.to_string())
    }
}

/// CRC-16/CCITT-FALSE: poly=0x1021, init=0xFFFF, no reflect, xorout=0.
///
/// Identical to the `crc.Configuration(width=16, polynomial=0x1021,
/// init_value=0xFFFF, final_xor_value=0x0000)` configuration the Python GDS
/// uses in `space_data_link.py`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-check our CRC-16/CCITT-FALSE against the well-known test vector
    /// for the ASCII string "123456789", which should yield 0x29B1.
    #[test]
    fn crc16_known_vector() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    }
}
