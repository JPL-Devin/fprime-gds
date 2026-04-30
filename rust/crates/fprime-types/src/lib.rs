//! F´ wire-format primitives.
//!
//! Implements the byte-level serialization and deserialization used by the
//! F Prime ground system.  Mirrors `fprime_gds.common.models.serialize`.
//!
//! All multi-byte integers are big-endian.  Floats use IEEE-754 big-endian.
//! `String` values are length-prefixed by a `U16` byte count followed by the
//! UTF-8 bytes.

#![deny(rust_2018_idioms)]

use std::fmt;

pub mod error;
pub mod time;
pub mod value;

pub use error::TypeError;
pub use time::TimeType;
pub use value::Value;

/// Trait for types that can be serialized into / deserialized from F´
/// wire format.
pub trait Serde: Sized {
    fn serialize(&self, out: &mut Vec<u8>);
    fn deserialize(data: &[u8], offset: usize) -> Result<(Self, usize), TypeError>;

    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize(&mut buf);
        buf
    }
}

/// Errors are produced when input does not have enough bytes, or when a value
/// is out of range.  Helper that ensures `data[offset..offset + n]` is in bounds.
fn need(data: &[u8], offset: usize, n: usize) -> Result<&[u8], TypeError> {
    data.get(offset..offset + n).ok_or(TypeError::ShortRead {
        needed: n,
        have: data.len().saturating_sub(offset),
    })
}

macro_rules! prim_impl {
    ($t:ty, $size:expr) => {
        impl Serde for $t {
            fn serialize(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_be_bytes());
            }
            fn deserialize(data: &[u8], offset: usize) -> Result<(Self, usize), TypeError> {
                let bytes = need(data, offset, $size)?;
                let mut arr = [0u8; $size];
                arr.copy_from_slice(bytes);
                Ok((<$t>::from_be_bytes(arr), $size))
            }
        }
    };
}

prim_impl!(u8, 1);
prim_impl!(u16, 2);
prim_impl!(u32, 4);
prim_impl!(u64, 8);
prim_impl!(i8, 1);
prim_impl!(i16, 2);
prim_impl!(i32, 4);
prim_impl!(i64, 8);
prim_impl!(f32, 4);
prim_impl!(f64, 8);

impl Serde for bool {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(if *self { 0xFF } else { 0x00 });
    }
    fn deserialize(data: &[u8], offset: usize) -> Result<(Self, usize), TypeError> {
        let byte = need(data, offset, 1)?[0];
        Ok((byte != 0, 1))
    }
}

/// F´ string: U16 BE length followed by UTF-8 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpString(pub String);

impl fmt::Display for FpString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serde for FpString {
    fn serialize(&self, out: &mut Vec<u8>) {
        let bytes = self.0.as_bytes();
        let len = bytes.len() as u16;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    }

    fn deserialize(data: &[u8], offset: usize) -> Result<(Self, usize), TypeError> {
        let (len, len_size) = u16::deserialize(data, offset)?;
        let len = len as usize;
        let bytes = need(data, offset + len_size, len)?;
        let s = std::str::from_utf8(bytes)
            .map_err(|e| TypeError::InvalidUtf8(e.to_string()))?
            .to_owned();
        Ok((FpString(s), len_size + len))
    }
}
