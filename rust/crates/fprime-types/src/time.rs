//! F´ `TimeType` — 11 bytes on the wire.
//!
//! Layout (big-endian):
//!
//! | bytes | field        | rust type |
//! |-------|--------------|-----------|
//! | 2     | time_base    | u16       |
//! | 1     | time_context | u8        |
//! | 4     | seconds      | u32       |
//! | 4     | useconds     | u32       |

use crate::{need, Serde, TypeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeType {
    pub time_base: u16,
    pub time_context: u8,
    pub seconds: u32,
    pub useconds: u32,
}

impl TimeType {
    pub const SIZE: usize = 11;

    pub fn new(time_base: u16, time_context: u8, seconds: u32, useconds: u32) -> Self {
        Self {
            time_base,
            time_context,
            seconds,
            useconds,
        }
    }
}

impl Serde for TimeType {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.time_base.to_be_bytes());
        out.push(self.time_context);
        out.extend_from_slice(&self.seconds.to_be_bytes());
        out.extend_from_slice(&self.useconds.to_be_bytes());
    }

    fn deserialize(data: &[u8], offset: usize) -> Result<(Self, usize), TypeError> {
        let bytes = need(data, offset, Self::SIZE)?;
        let time_base = u16::from_be_bytes([bytes[0], bytes[1]]);
        let time_context = bytes[2];
        let seconds = u32::from_be_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]);
        let useconds = u32::from_be_bytes([bytes[7], bytes[8], bytes[9], bytes[10]]);
        Ok((
            Self {
                time_base,
                time_context,
                seconds,
                useconds,
            },
            Self::SIZE,
        ))
    }
}

impl std::fmt::Display for TimeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:06}", self.seconds, self.useconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = TimeType::new(2, 0, 1_700_000_000, 123_456);
        let mut buf = Vec::new();
        t.serialize(&mut buf);
        assert_eq!(buf.len(), TimeType::SIZE);
        let (r, n) = TimeType::deserialize(&buf, 0).unwrap();
        assert_eq!(n, TimeType::SIZE);
        assert_eq!(r, t);
    }
}
